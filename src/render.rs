// This file is a part of SORT-rs, a Rust port of SORT (Simple Open Ray Tracing).
// Original work Copyright (c) 2011-2023 by Jiayin Cao, GPLv3.

//! Multi-threaded rendering loop and image output.

use crate::camera::Camera;
use crate::integrator::PathTracing;
use crate::math::*;
use crate::rng::Rng;
use crate::scene::Scene;
use rayon::prelude::*;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Debug)]
pub struct RenderSettings {
    pub samples_per_pixel: u32,
    pub max_depth: u32,
    /// Clamp the radiance of each sample to this value. Removes fireflies at the cost of bias.
    pub clamp: Option<f32>,
    /// Blur of the surfaces met after a rough bounce, see `PathTracing::filter_glossy`.
    pub filter_glossy: f32,
    pub seed: u64,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self { samples_per_pixel: 64, max_depth: 6, clamp: None, filter_glossy: 0.5, seed: 0 }
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
    let integrator =
        PathTracing { max_recursive_depth: settings.max_depth, filter_glossy: settings.filter_glossy };
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
    use crate::material::Material;
    use crate::scene::SceneBuilder;

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
}
