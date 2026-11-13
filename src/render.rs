// This file is a part of SORT-rs, a Rust port of SORT (Simple Open Ray Tracing).
// Original work Copyright (c) 2011-2023 by Jiayin Cao, GPLv3.

//! Multi-threaded rendering loop and image output.

use crate::camera::Camera;
use crate::integrator::{AmbientOcclusion, PathTracing};
use crate::math::*;
use crate::rng::Rng;
use crate::scene::Scene;
use rayon::prelude::*;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

/// What computes the radiance of a camera ray.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Integrator {
    /// Global illumination, see `integrator::PathTracing`.
    PathTracing,
    /// Clay render, see `integrator::AmbientOcclusion`.
    AmbientOcclusion { max_distance: f32 },
}

#[derive(Clone, Debug)]
pub struct RenderSettings {
    pub samples_per_pixel: u32,
    /// Path tracing only.
    pub max_depth: u32,
    /// Clamp the radiance of each sample to this value. Removes fireflies at the cost of bias.
    pub clamp: Option<f32>,
    /// Blur of the surfaces met after a rough bounce, see `PathTracing::filter_glossy`. Path
    /// tracing only.
    pub filter_glossy: f32,
    pub seed: u64,
    pub integrator: Integrator,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            samples_per_pixel: 64,
            max_depth: 6,
            clamp: None,
            filter_glossy: 0.5,
            seed: 0,
            integrator: Integrator::PathTracing,
        }
    }
}

/// The integrator, built once per render.
enum Active {
    PathTracing(PathTracing),
    AmbientOcclusion(AmbientOcclusion),
}

impl Active {
    #[inline]
    fn li(&self, ray: &Ray, scene: &Scene, rng: &mut Rng) -> Spectrum {
        match self {
            Active::PathTracing(pt) => pt.li(ray, scene, rng),
            Active::AmbientOcclusion(ao) => ao.li(ray, scene, rng),
        }
    }
}

/// Render the scene. Returns linear radiance, row by row from the top of the image.
/// `progress` is called from worker threads with the fraction of finished rows.
pub fn render(
    scene: &Scene,
    camera: &Camera,
    settings: &RenderSettings,
    progress: impl Fn(f32) + Sync,
) -> Vec<Spectrum> {
    let (width, height) = (camera.width as usize, camera.height as usize);
    let integrator = match settings.integrator {
        Integrator::PathTracing => Active::PathTracing(PathTracing {
            max_recursive_depth: settings.max_depth,
            filter_glossy: settings.filter_glossy,
        }),
        Integrator::AmbientOcclusion { max_distance } => {
            Active::AmbientOcclusion(AmbientOcclusion { max_distance })
        }
    };
    let spp = settings.samples_per_pixel.max(1);
    let done = AtomicUsize::new(0);

    let mut pixels = vec![Spectrum::ZERO; width * height];
    pixels.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let mut rng = Rng::new(settings.seed, y as u64);
        for (x, pixel) in row.iter_mut().enumerate() {
            let mut sum = Spectrum::ZERO;
            for _ in 0..spp {
                let (jx, jy) = rng.pair();
                let ray = camera.generate_ray(x as f32 + jx, y as f32 + jy, rng.pair());
                let mut radiance = integrator.li(&ray, scene, &mut rng);
                if !radiance.is_finite() {
                    continue;
                }
                if let Some(limit) = settings.clamp {
                    let max = radiance.max_element();
                    if max > limit {
                        radiance *= limit / max;
                    }
                }
                sum += radiance;
            }
            *pixel = sum / spp as f32;
        }
        let finished = done.fetch_add(1, Ordering::Relaxed) + 1;
        progress(finished as f32 / height as f32);
    });
    pixels
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToneMap {
    /// Clamp to [0,1].
    None,
    /// ACES filmic curve (Narkowicz approximation).
    Aces,
}

#[inline]
fn aces(x: f32) -> f32 {
    saturate((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14))
}

#[inline]
fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Save an image. `.exr` and `.hdr` files store linear radiance, every other format is tone
/// mapped and sRGB encoded.
pub fn save_image(
    path: &Path,
    pixels: &[Spectrum],
    width: u32,
    height: u32,
    exposure: f32,
    tone_map: ToneMap,
) -> image::ImageResult<()> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext == "exr" || ext == "hdr" {
        let data: Vec<f32> = pixels.iter().flat_map(|p| (*p * exposure).to_array()).collect();
        let img = image::Rgb32FImage::from_raw(width, height, data).expect("pixel buffer size mismatch");
        return image::DynamicImage::ImageRgb32F(img).save(path);
    }

    let data: Vec<u8> = pixels
        .iter()
        .flat_map(|p| {
            (*p * exposure).to_array().map(|c| {
                let c = match tone_map {
                    ToneMap::None => saturate(c),
                    ToneMap::Aces => aces(c.max(0.0)),
                };
                (linear_to_srgb(c) * 255.0 + 0.5) as u8
            })
        })
        .collect();
    image::RgbImage::from_raw(width, height, data).expect("pixel buffer size mismatch").save(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::light::{Light, Sky};
    use crate::material::{AlphaMode, Cloth, Material, Texture};
    use crate::medium::{Fog, HenyeyGreenstein, Medium};
    use crate::scene::SceneBuilder;
    use std::sync::Arc;

    fn floor(builder: &mut SceneBuilder, material: u32) {
        let s = 500.0;
        builder.add_quad(
            [Vec3::new(-s, 0.0, s), Vec3::new(s, 0.0, s), Vec3::new(s, 0.0, -s), Vec3::new(-s, 0.0, -s)],
            material,
        );
    }

    fn center_pixel(scene: &Scene, spp: u32, depth: u32) -> Spectrum {
        // look straight down at the floor through a narrow field of view
        let camera = Camera::look_at(Vec3::new(0.0, 3.0, 0.0), Vec3::ZERO, Vec3::Z, 0.01, 3, 3);
        let settings = RenderSettings { samples_per_pixel: spp, max_depth: depth, ..Default::default() };
        render(scene, &camera, &settings, |_| {})[4]
    }

    /// A diffuse plane under a uniform sky of radiance 1 reflects exactly its albedo.
    #[test]
    fn white_furnace_under_uniform_sky() {
        let albedo = Spectrum::new(0.7, 0.5, 0.3);
        let mut builder = SceneBuilder::new();
        // pure Lambertian response: no specular, fully rough
        let material = builder.add_material(Material { base_color: albedo, specular: 0.0, roughness: 1.0, ..Default::default() });
        floor(&mut builder, material);
        builder.add_light(Light::Sky(Sky { intensity: Spectrum::ONE, env: None, rotation: 0.0 }));
        let scene = builder.build();

        let result = center_pixel(&scene, 4000, 6);
        // Disney diffuse with retro-reflection is not exactly Lambertian, allow some slack
        assert!((result - albedo).abs().max_element() < 0.06, "got {result:?}, expected {albedo:?}");
    }

    /// Coats, sheens and cloths never reflect more than they receive, under a uniform sky of
    /// radiance 1, and a clear coat over a white diffuse base keeps nearly all of its light.
    #[test]
    fn layered_and_cloth_furnace() {
        let white = Material { base_color: Spectrum::ONE, specular: 0.0, roughness: 1.0, ..Default::default() };
        let measure = |material: Material| {
            let mut builder = SceneBuilder::new();
            let id = builder.add_material(material);
            floor(&mut builder, id);
            builder.add_light(Light::Sky(Sky { intensity: Spectrum::ONE, env: None, rotation: 0.0 }));
            center_pixel(&builder.build(), 4000, 6).max_element()
        };

        let bare = measure(white.clone());
        let coated = measure(Material { coat: 1.0, coat_roughness: 0.2, ..white.clone() });
        // The coat moves light from the base to its own reflection; the light it reflects
        // back down is returned to the base (exactly so for a Lambertian one). What is left is
        // what the GGX top loses to masking, a fraction of a percent here, plus noise: within
        // 3% of all the light. The bare Disney base reflects a few percent more than it
        // receives seen from above (its retro-reflection), which the coat hides: it is
        // seen through the layer at angles where that term is negligible.
        assert!(coated > 0.97 && coated < 1.02, "coated {coated}");
        assert!(coated > bare - 0.05, "coated {coated}, bare {bare}");

        let sheen = measure(Material { sheen_color: Spectrum::ONE, sheen_roughness: 0.5, ..white.clone() });
        assert!(sheen > 0.1 && sheen < 1.02, "sheen {sheen}");
        let cloths = [
            Cloth::Fabric { roughness: 0.3 },
            Cloth::Distribution { roughness: 0.5, specular: 0.5, specular_tint: 0.0 },
            Cloth::Distribution { roughness: 0.8, specular: 1.0, specular_tint: 1.0 },
        ];
        for cloth in cloths {
            let value = measure(Material { cloth: Some(cloth), ..white.clone() });
            assert!(value > 0.1 && value < 1.02, "{cloth:?}: {value}");
        }
    }

    /// A sheet black on the front and white on the back (`Material::back_material`).
    #[test]
    fn back_material_shades_the_back_face() {
        let mut builder = SceneBuilder::new();
        let back = builder.add_material(Material { base_color: Spectrum::splat(0.5), specular: 0.0, roughness: 1.0, ..Default::default() });
        let front = builder.add_material(Material {
            base_color: Spectrum::ZERO,
            specular: 0.0,
            back_material: Some(back),
            ..Default::default()
        });
        let broken = builder.add_material(Material { back_material: Some(99), ..Default::default() });
        // facing up
        floor(&mut builder, front);
        builder.add_light(Light::Sky(Sky { intensity: Spectrum::ONE, env: None, rotation: 0.0 }));
        let scene = builder.build();
        assert_eq!(scene.materials[broken as usize].back_material, None);

        let settings = RenderSettings { samples_per_pixel: 2000, max_depth: 6, ..Default::default() };
        let look = |eye: Vec3| {
            let camera = Camera::look_at(eye, Vec3::ZERO, Vec3::Z, 0.01, 3, 3);
            render(&scene, &camera, &settings, |_| {})[4]
        };
        let above = look(Vec3::new(0.0, 3.0, 0.0));
        let below = look(Vec3::new(0.0, -3.0, 0.0));
        assert!(above.max_element() < 1e-3, "front: {above}");
        assert!((below - Spectrum::splat(0.5)).abs().max_element() < 0.04, "back: {below}");
    }

    /// A distant light at an angle gives E * cos / pi * albedo for a diffuse surface.
    #[test]
    fn distant_light_irradiance() {
        let mut builder = SceneBuilder::new();
        let material = builder.add_material(Material { base_color: Spectrum::splat(0.5), specular: 0.0, roughness: 1.0, ..Default::default() });
        floor(&mut builder, material);
        let to_light = Vec3::new(1.0, 1.0, 0.0).normalize();
        builder.add_light(Light::Distant { dir: -to_light, intensity: Spectrum::splat(PI) });
        let scene = builder.build();

        let result = center_pixel(&scene, 64, 1);
        let expected = 0.5 * to_light.y;
        // looking straight down the Disney diffuse Fresnel factor (1 - 0.5 * FI) applies
        let fi = (1.0 - to_light.y).powi(5);
        let rr = 2.0 * (to_light + Vec3::Y).normalize().y.powi(2);
        let expected = expected * ((1.0 - 0.5 * fi) + rr * fi);
        assert!((result.x - expected).abs() < 0.01, "got {}, expected {expected}", result.x);
    }

    /// Light sampling and BSDF sampling of an area light must agree with the analytic result:
    /// a large emissive ceiling of radiance L over a diffuse floor behaves like a uniform sky.
    #[test]
    fn area_light_matches_analytic() {
        let mut builder = SceneBuilder::new();
        let diffuse = builder.add_material(Material { base_color: Spectrum::splat(0.6), specular: 0.0, roughness: 1.0, ..Default::default() });
        let emitter = builder.add_material(Material { emissive: Spectrum::splat(2.0), base_color: Spectrum::ZERO, ..Default::default() });
        floor(&mut builder, diffuse);
        let (s, h) = (4000.0, 10.0);
        // facing down
        builder.add_quad(
            [Vec3::new(-s, h, -s), Vec3::new(s, h, -s), Vec3::new(s, h, s), Vec3::new(-s, h, s)],
            emitter,
        );
        let scene = builder.build();

        let result = center_pixel(&scene, 4000, 2);
        let expected = 0.6 * 2.0;
        assert!((result.x - expected).abs() < 0.08 * expected, "got {}, expected {expected}", result.x);
    }

    /// Sky light reaching a floor through a thin glass pane, seen through that pane, is
    /// attenuated twice, and the pane adds its own reflection of the sky. Counting the sky both through the shadow rays that cross the glass
    /// and again when a path goes through it would give more than that.
    #[test]
    fn sky_through_thin_glass_is_not_counted_twice() {
        let build = |with_glass: bool| {
            let mut builder = SceneBuilder::new();
            let diffuse = builder.add_material(Material { base_color: Spectrum::splat(0.5), specular: 0.0, roughness: 1.0, ..Default::default() });
            floor(&mut builder, diffuse);
            if with_glass {
                let glass = builder.add_material(Material { base_color: Spectrum::ONE, transmission: 1.0, thin: true, roughness: 0.0, ..Default::default() });
                let s = 500.0;
                builder.add_quad(
                    [Vec3::new(-s, 1.0, s), Vec3::new(s, 1.0, s), Vec3::new(s, 1.0, -s), Vec3::new(-s, 1.0, -s)],
                    glass,
                );
            }
            builder.add_light(Light::Sky(Sky { intensity: Spectrum::ONE, env: None, rotation: 0.0 }));
            builder.build()
        };

        // Depths chosen so that both renders hold exactly the sky light the floor receives:
        // crossing the pane takes a bounce on the way in and another on the way out.
        let open = center_pixel(&build(false), 4000, 1).x;
        let glazed = center_pixel(&build(true), 8000, 3).x;
        // plus the sky mirrored by the pane itself, 4% at normal incidence
        let expected = open * 0.96 * 0.96 + 0.04;
        assert!((open - 0.5).abs() < 0.04, "open floor: {open}");
        assert!((glazed - expected).abs() < 0.04 * expected, "behind glass: {glazed}, expected about {expected}");
    }

    /// Portals and sun points only change how light is sampled, never the result: a room lit
    /// by the sky and the sun through a skylight renders the same with and without them.
    #[test]
    fn portals_do_not_change_the_result() {
        let build = |with_portal: bool| {
            let mut b = SceneBuilder::new();
            let diffuse = b.add_material(Material { base_color: Spectrum::splat(0.6), specular: 0.0, roughness: 1.0, ..Default::default() });
            let p = Vec3::new;
            let (s, h) = (2.0, 3.0);
            b.add_quad([p(-s, 0.0, s), p(s, 0.0, s), p(s, 0.0, -s), p(-s, 0.0, -s)], diffuse);
            b.add_quad([p(-s, 0.0, -s), p(s, 0.0, -s), p(s, h, -s), p(-s, h, -s)], diffuse);
            b.add_quad([p(s, 0.0, s), p(-s, 0.0, s), p(-s, h, s), p(s, h, s)], diffuse);
            b.add_quad([p(s, 0.0, -s), p(s, 0.0, s), p(s, h, s), p(s, h, -s)], diffuse);
            b.add_quad([p(-s, 0.0, s), p(-s, 0.0, -s), p(-s, h, -s), p(-s, h, s)], diffuse);
            // ceiling with a skylight over x in [0.5, 1.5], z in [-0.5, 0.5]
            let ceiling = |x0: f32, x1: f32, z0: f32, z1: f32, b: &mut SceneBuilder| {
                b.add_quad([p(x0, h, z0), p(x1, h, z0), p(x1, h, z1), p(x0, h, z1)], diffuse);
            };
            ceiling(-s, 0.5, -s, s, &mut b);
            ceiling(1.5, s, -s, s, &mut b);
            ceiling(0.5, 1.5, -s, -0.5, &mut b);
            ceiling(0.5, 1.5, 0.5, s, &mut b);
            if with_portal {
                b.add_portal([p(0.5, h, -0.5), p(1.5, h, -0.5), p(1.5, h, 0.5), p(0.5, h, 0.5)]);
            }
            b.add_light(Light::Sky(Sky { intensity: Spectrum::ONE, env: None, rotation: 0.0 }));
            // the sun lands on the floor away from the pixel that is measured
            b.add_light(Light::Distant { dir: Vec3::new(0.3, -1.0, 0.0).normalize(), intensity: Spectrum::splat(10.0) });
            b.build()
        };
        let measure = |scene: &Scene, spp: u32| {
            let camera = Camera::look_at(Vec3::new(0.0, 2.5, 0.0), Vec3::ZERO, Vec3::Z, 0.01, 3, 3);
            let settings = RenderSettings { samples_per_pixel: spp, max_depth: 5, filter_glossy: 0.0, ..Default::default() };
            render(scene, &camera, &settings, |_| {})[4].x
        };

        let with_portal = build(true);
        assert_eq!(with_portal.portal_count(), 2);
        let plain = measure(&build(false), 60_000);
        let guided = measure(&with_portal, 6_000);
        assert!(plain > 0.02, "the room must be lit: {plain}");
        assert!((guided - plain).abs() < 0.04 * plain, "with portals {guided}, without {plain}");
    }

    /// A thin glass pane attenuates what is behind it instead of blocking it.
    #[test]
    fn thin_glass_lets_light_through() {
        let build = |with_glass: bool| {
            let mut builder = SceneBuilder::new();
            let diffuse = builder.add_material(Material { base_color: Spectrum::splat(0.5), specular: 0.0, roughness: 1.0, ..Default::default() });
            floor(&mut builder, diffuse);
            if with_glass {
                let glass = builder.add_material(Material {
                    base_color: Spectrum::ONE,
                    transmission: 1.0,
                    thin: true,
                    roughness: 0.0,
                    ..Default::default()
                });
                let s = 500.0;
                builder.add_quad(
                    [Vec3::new(-s, 1.0, s), Vec3::new(s, 1.0, s), Vec3::new(s, 1.0, -s), Vec3::new(-s, 1.0, -s)],
                    glass,
                );
            }
            // tilted so that the camera does not sit in the mirror reflection of the light
            let dir = Vec3::new(-1.0, -1.0, 0.0).normalize();
            builder.add_light(Light::Distant { dir, intensity: Spectrum::splat(PI) });
            builder.build()
        };

        // depth 2 leaves out the inter-reflection between the floor and the pane
        let open = center_pixel(&build(false), 256, 2).x;
        let glazed = center_pixel(&build(true), 4000, 2).x;
        assert!(open > 0.3, "open floor: {open}");
        // light crosses the pane twice: on the way down and on the way back to the camera
        let expected = open * 0.96 * 0.96;
        assert!((glazed - expected).abs() < 0.05 * expected, "behind glass: {glazed}, expected about {expected}");
    }

    /// The pixel straight below a camera at `eye` looking down.
    fn pixel_below(scene: &Scene, eye: Vec3, spp: u32, depth: u32) -> Spectrum {
        let camera = Camera::look_at(eye, eye - Vec3::Y, Vec3::Z, 0.01, 3, 3);
        let settings = RenderSettings { samples_per_pixel: spp, max_depth: depth, ..Default::default() };
        render(scene, &camera, &settings, |_| {})[4]
    }

    /// A masked quad is a hole where its alpha is below the cutoff: it casts the shadow of its
    /// opaque half only, and the camera sees the floor through the other half.
    #[test]
    fn alpha_cutout_is_a_hole() {
        let build = |with_quad: bool| {
            let mut builder = SceneBuilder::new();
            let diffuse = builder.add_material(Material { base_color: Spectrum::splat(0.5), specular: 0.0, roughness: 1.0, ..Default::default() });
            floor(&mut builder, diffuse);
            if with_quad {
                // opaque for u in (0, 0.5), a hole for u in (0.5, 1): the bilinear filter
                // crosses the cutoff halfway between the texel centers at u = 0.25 and 0.75
                let texture = Arc::new(Texture {
                    width: 2,
                    height: 1,
                    data: vec![[255, 255, 255, 255], [255, 255, 255, 0]],
                    srgb: true,
                });
                let leaf = builder.add_material(Material {
                    base_color: Spectrum::splat(0.9),
                    base_color_tex: Some(texture.into()),
                    alpha_mode: AlphaMode::Mask { cutoff: 0.5 },
                    ..Default::default()
                });
                // u goes from x = -2 to x = 2
                let s = 2.0;
                builder.add_quad(
                    [Vec3::new(-s, 1.0, s), Vec3::new(s, 1.0, s), Vec3::new(s, 1.0, -s), Vec3::new(-s, 1.0, -s)],
                    leaf,
                );
            }
            builder.add_light(Light::Distant { dir: -Vec3::Y, intensity: Spectrum::splat(PI) });
            builder.build()
        };
        let (open, scene) = (build(false), build(true));
        let (opaque, hole) = (Vec3::new(-1.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0));
        let below_quad = Vec3::new(0.0, 0.5, 0.0);

        let lit = pixel_below(&open, hole + below_quad, 64, 1).x;
        assert!(lit > 0.3, "open floor: {lit}");
        let shadow = pixel_below(&scene, opaque + below_quad, 64, 1).x;
        assert!(shadow == 0.0, "under the opaque half: {shadow}");
        let through = pixel_below(&scene, hole + below_quad, 64, 1).x;
        assert!((through - lit).abs() < 1e-3 * lit, "under the hole: {through}, open floor {lit}");

        // seen from above, the camera ray goes through the hole down to the floor
        let above = Vec3::new(0.0, 3.0, 0.0);
        let hit = scene.intersect(&Ray::new(hole + above, -Vec3::Y, 0.0, f32::MAX)).unwrap();
        assert!((hit.t - 3.0).abs() < 1e-4, "hit at {}", hit.t);
        let seen = pixel_below(&scene, hole + above, 64, 1).x;
        assert!((seen - lit).abs() < 1e-3 * lit, "through the hole: {seen}, open floor {lit}");
        let hit = scene.intersect(&Ray::new(opaque + above, -Vec3::Y, 0.0, f32::MAX)).unwrap();
        assert!((hit.t - 2.0).abs() < 1e-4, "hit at {}", hit.t);
    }

    /// A black quad covering half of each point lets half of the light through on the way
    /// down to the floor and half of it on the way back to the camera.
    #[test]
    fn alpha_blend_is_partial_coverage() {
        let build = |with_quad: bool| {
            let mut builder = SceneBuilder::new();
            let diffuse = builder.add_material(Material { base_color: Spectrum::splat(0.5), specular: 0.0, roughness: 1.0, ..Default::default() });
            floor(&mut builder, diffuse);
            if with_quad {
                let sheer = builder.add_material(Material {
                    base_color: Spectrum::ZERO,
                    specular: 0.0,
                    roughness: 1.0,
                    alpha: 0.5,
                    alpha_mode: AlphaMode::Blend,
                    ..Default::default()
                });
                let s = 500.0;
                builder.add_quad(
                    [Vec3::new(-s, 1.0, s), Vec3::new(s, 1.0, s), Vec3::new(s, 1.0, -s), Vec3::new(-s, 1.0, -s)],
                    sheer,
                );
            }
            let dir = Vec3::new(-1.0, -1.0, 0.0).normalize();
            builder.add_light(Light::Distant { dir, intensity: Spectrum::splat(PI) });
            builder.build()
        };

        // as for thin glass, depth 2 is the floor's direct light seen through the quad
        let open = center_pixel(&build(false), 256, 2).x;
        let covered = center_pixel(&build(true), 8000, 2).x;
        assert!(open > 0.3, "open floor: {open}");
        let expected = open * 0.25;
        assert!((covered - expected).abs() < 0.05 * expected, "under the quad: {covered}, expected about {expected}");
    }

    fn ao_center_pixel(scene: &Scene, spp: u32, max_distance: f32) -> Spectrum {
        let camera = Camera::look_at(Vec3::new(0.0, 3.0, 0.0), Vec3::ZERO, Vec3::Z, 0.01, 3, 3);
        let settings = RenderSettings {
            samples_per_pixel: spp,
            integrator: Integrator::AmbientOcclusion { max_distance },
            ..Default::default()
        };
        render(scene, &camera, &settings, |_| {})[4]
    }

    /// Nothing around an open floor occludes it: ambient occlusion is exactly white.
    #[test]
    fn ambient_occlusion_of_an_open_floor_is_white() {
        let mut builder = SceneBuilder::new();
        let material = builder.add_material(Material::default());
        floor(&mut builder, material);
        let scene = builder.build();
        assert_eq!(ao_center_pixel(&scene, 64, 3.0), Spectrum::ONE);
    }

    /// A ceiling counts only when it is closer than the occlusion distance.
    #[test]
    fn ambient_occlusion_respects_the_distance() {
        let mut builder = SceneBuilder::new();
        let material = builder.add_material(Material::default());
        floor(&mut builder, material);
        let (s, h) = (4000.0, 10.0);
        builder.add_quad(
            [Vec3::new(-s, h, -s), Vec3::new(s, h, -s), Vec3::new(s, h, s), Vec3::new(-s, h, s)],
            material,
        );
        let scene = builder.build();

        assert_eq!(ao_center_pixel(&scene, 64, 0.5 * h).x, 1.0);
        // only directions within a hair of the horizon escape a ceiling that wide
        let closed = ao_center_pixel(&scene, 1000, 1e5 * h).x;
        assert!(closed < 0.01, "under the ceiling: {closed}");
    }

    /// Thin glass between the camera and the floor, and over the floor, attenuates instead of
    /// occluding.
    #[test]
    fn ambient_occlusion_sees_through_thin_glass() {
        let mut builder = SceneBuilder::new();
        let diffuse = builder.add_material(Material::default());
        floor(&mut builder, diffuse);
        let glass = builder.add_material(Material {
            base_color: Spectrum::ONE,
            transmission: 1.0,
            thin: true,
            roughness: 0.0,
            ..Default::default()
        });
        let s = 500.0;
        builder.add_quad(
            [Vec3::new(-s, 1.0, s), Vec3::new(s, 1.0, s), Vec3::new(s, 1.0, -s), Vec3::new(-s, 1.0, -s)],
            glass,
        );
        let scene = builder.build();

        let glazed = ao_center_pixel(&scene, 256, 3.0).x;
        // once through the pane for the camera, once for every occlusion ray
        assert!(glazed > 0.8 && glazed <= 1.0, "behind glass: {glazed}");
    }

    /// Fog that only scatters neither adds nor removes light: under a uniform sky of radiance
    /// 1, with nothing else in sight, a box of it is invisible.
    #[test]
    fn scattering_fog_furnace() {
        for g in [0.0, 0.7] {
            let mut builder = SceneBuilder::new();
            // the scene needs a triangle, out of the way
            let black = builder.add_material(Material { base_color: Spectrum::ZERO, specular: 0.0, ..Default::default() });
            builder.add_box(Vec3::splat(900.0), Vec3::splat(900.1), black);
            builder.add_light(Light::Sky(Sky { intensity: Spectrum::ONE, env: None, rotation: 0.0 }));
            builder.fog = Some(Fog {
                medium: Medium::scattering(1.0, Spectrum::ONE, g),
                min: Vec3::splat(-1.0),
                max: Vec3::splat(1.0),
            });
            let result = center_pixel(&builder.build(), 4000, 64);
            assert!((result - Spectrum::ONE).abs().max_element() < 0.02, "g={g}: {result:?}");
        }
    }

    /// A layer of absorbing fog one meter thick over a glowing floor dims it by Beer's law,
    /// and a tinted glass slab one meter thick by the color it is given at that distance.
    #[test]
    fn absorption_follows_the_distance_travelled() {
        let glowing = Material { base_color: Spectrum::ZERO, specular: 0.0, emissive: Spectrum::ONE, ..Default::default() };

        let sigma_a = Spectrum::new(0.5, 1.0, 2.0);
        let mut builder = SceneBuilder::new();
        let id = builder.add_material(glowing.clone());
        floor(&mut builder, id);
        let medium = Medium { sigma_a, sigma_s: Spectrum::ZERO, phase: HenyeyGreenstein::new(0.0) };
        let inf = f32::INFINITY;
        builder.fog = Some(Fog { medium, min: Vec3::new(-inf, 1.0, -inf), max: Vec3::new(inf, 2.0, inf) });
        let result = center_pixel(&builder.build(), 64, 6);
        let expected = sigma_a.map(|s| (-s).exp());
        assert!((result - expected).abs().max_element() < 1e-3, "fog: {result:?} vs {expected:?}");

        let color = Spectrum::new(0.9, 0.6, 0.3);
        let through_slab = |medium: Option<Medium>| {
            let mut builder = SceneBuilder::new();
            let id = builder.add_material(glowing.clone());
            floor(&mut builder, id);
            let glass = builder.add_material(Material {
                base_color: Spectrum::ONE,
                transmission: 1.0,
                roughness: 0.0,
                medium,
                ..Default::default()
            });
            builder.add_box(Vec3::new(-50.0, 1.0, -50.0), Vec3::new(50.0, 2.0, 50.0), glass);
            center_pixel(&builder.build(), 2000, 12)
        };
        let clear = through_slab(None);
        let tinted = through_slab(Some(Medium::absorbing(color, 1.0)));
        assert!(clear.min_element() > 0.8, "clear slab: {clear:?}");
        // light reflected back and forth inside the slab travels further, a small part of it
        let ratio = tinted / clear;
        assert!((ratio - color).abs().max_element() < 0.03, "slab: {ratio:?} vs {color:?}");
    }

    /// Fog lit by a sun, seen from the side: a path that scatters once and the light sampled
    /// from that vertex must give what the integral of single scattering does.
    #[test]
    fn single_scattering_matches_the_integral() {
        let (sigma, albedo, irradiance) = (0.4f32, 0.6f32, 5.0f32);
        let mut builder = SceneBuilder::new();
        let black = builder.add_material(Material { base_color: Spectrum::ZERO, specular: 0.0, ..Default::default() });
        builder.add_box(Vec3::splat(900.0), Vec3::splat(900.1), black);
        // the sun shines straight down on a layer between y = -1 and y = 1
        builder.add_light(Light::Distant { dir: -Vec3::Y, intensity: Spectrum::splat(irradiance) });
        let inf = f32::INFINITY;
        builder.fog = Some(Fog {
            medium: Medium::scattering(sigma, Spectrum::splat(albedo), 0.0),
            min: Vec3::new(-inf, -1.0, -inf),
            max: Vec3::new(inf, 1.0, inf),
        });
        let scene = builder.build();

        // look straight up through the layer from below it, away from the sun's direction of
        // travel: the camera ray and the sun ray share the vertical line
        let camera = Camera::look_at(Vec3::new(0.0, -3.0, 0.0), Vec3::ZERO, Vec3::Z, 0.01, 3, 3);
        // one bounce: single scattering only
        let settings = RenderSettings { samples_per_pixel: 20000, max_depth: 1, ..Default::default() };
        let result = render(&scene, &camera, &settings, |_| {})[4].x;

        // A point at height y gets the sun through (1 - y) of fog and is seen through (y + 1)
        // of it: the two always add up to the thickness of the layer.
        let thickness = 2.0;
        let phase = 1.0 / (4.0 * PI);
        let expected = irradiance * sigma * albedo * phase * thickness * (-sigma * thickness).exp();
        assert!((result - expected).abs() < 0.02 * expected, "{result} vs {expected}");
    }
}
