// This file is a part of SORT-rs, a Rust port of SORT (Simple Open Ray Tracing).
// Original work Copyright (c) 2011-2023 by Jiayin Cao, GPLv3.

//! Unidirectional path tracing with next event estimation and multiple importance sampling.
//!
//! It started as a port of `src/integrator/pathtracing.cpp` and `integratormethod.cpp`
//! (without participating media and sub-surface scattering) and departs from them in how
//! lighting is sampled, which is where most of the noise of an interior comes from:
//!
//! - SORT picks one light per vertex proportionally to its power. Here the sun, the sky, the
//!   punctual lights and the emissive surfaces each get their own shadow rays: they do not
//!   compete, so a sun hidden behind a wall does not starve the lamp of samples. Among the
//!   punctual lights, and among the emissive surfaces, several candidates are drawn and one
//!   is kept proportionally to what it would contribute at the shaded point if nothing
//!   blocked it (resampled importance sampling), so a lamp next to a wall is not sampled as
//!   rarely as its share of the scene's power.
//! - The sky is sampled through the portals of the scene (windows), not only over the whole
//!   sphere, most of which is blocked by walls when seen from inside a room.
//!
//! Lights whose visibility differs a lot must not be resampled against each other: the
//! candidate is kept before its shadow ray is traced, so one that is bright but hidden (the
//! sun behind a wall, the part of the sky that is not behind a window) keeps winning and
//! returning nothing.
//! - SORT traces a second, dedicated BSDF ray per vertex to combine it with light sampling.
//!   Here the ray that continues the path does that job: emitters and the sky it finds are
//!   weighted against light sampling when it lands. One ray less per vertex, and every light
//!   benefits instead of only the one that was picked.
//! - A patch of sun on the floor lights the whole room, yet a path only finds it by chance.
//!   Each path gets a point lit by the sun through a portal and connects all its vertices to
//!   it, weighted against the paths that reach such a point by themselves.
//! - Light reflected by a polished surface onto a matte one (a caustic) is found by a path
//!   tracer so rarely that it shows up as isolated bright pixels. Once a path has bounced off
//!   a rough surface, the surfaces it meets next are made at least as blurry as that bounce
//!   was (`filter_glossy`): the caustic is blurred instead of scattered as fireflies. This
//!   is the one change that is not exact, and it can be turned off.
//!
//! `AmbientOcclusion`, ported from `src/integrator/ao.cpp`, renders the "clay" view
//! architects use: no materials and no lights, every surface white and darkened by the
//! geometry around it within a given distance. Unlike SORT, thin glass attenuates instead of
//! blocking, both for the camera and for the occlusion rays, so a glazed room is not black;
//! alpha blended surfaces let through the part of the rays they do not cover.

use crate::bsdf::ScatteringEvent;
use crate::light::Light;
use crate::math::*;
use crate::rng::Rng;
use crate::scene::{Hit, Scene, SunPoint, LIGHT_CANDIDATES};

pub struct PathTracing {
    /// Maximum number of bounces. This introduces bias, raise it to reduce the side effect.
    pub max_recursive_depth: u32,
    /// How much surfaces are blurred after a rough bounce, to tame caustics. 1 makes them as
    /// blurry as the bounce was, 0 renders caustics exactly (and noisily). What the camera
    /// sees directly, or through mirrors and glass, is never affected. Rough microfacet
    /// lobes reflect a little less, so this darkens a scene full of polished surfaces by a
    /// few percent.
    pub filter_glossy: f32,
}

impl Default for PathTracing {
    fn default() -> Self {
        Self { max_recursive_depth: 6, filter_glossy: 0.5 }
    }
}

/// Width (GGX alpha) of the lobe a direction of pdf `pdf` is typical of: a GGX lobe peaks at
/// about `1 / (4 pi alpha^2)`, a diffuse bounce gives an alpha around 0.6.
#[inline]
fn lobe_width(filter_glossy: f32, pdf: f32) -> f32 {
    if filter_glossy <= 0.0 {
        return 0.0;
    }
    (filter_glossy * (1.0 / (4.0 * PI * pdf)).sqrt()).min(1.0)
}

/// The last vertex of a path where the direction was sampled from a non-delta distribution,
/// and the pdf of that direction.
type Scatter = Option<(Vec3, f32)>;

struct Candidate {
    /// Unshadowed contribution divided by its luminance.
    contribution: Spectrum,
    wi: Vec3,
    density: f32,
    delta: bool,
    tmin: f32,
    tmax: f32,
}

/// Keeps one of the light candidates of a vertex, proportionally to its unshadowed
/// contribution.
struct Reservoir<'a> {
    se: &'a ScatteringEvent,
    wo: Vec3,
    weight_sum: f32,
    chosen: Option<Candidate>,
}

impl<'a> Reservoir<'a> {
    /// Offer a candidate direction `wi` carrying radiance `li`. `density` is the density
    /// w.r.t. solid angle with which direct lighting generates this direction.
    fn new(se: &'a ScatteringEvent, wo: Vec3) -> Self {
        Self { se, wo, weight_sum: 0.0, chosen: None }
    }

    /// Trace the shadow ray of the candidate that was kept.
    fn resolve(self, p: Vec3, scene: &Scene) -> Spectrum {
        let Some(c) = self.chosen else {
            return Spectrum::ZERO;
        };
        // the continuation of the path may find the same light, weight one against the other
        let mis = if c.delta { 1.0 } else { mis_factor(c.density, self.se.pdf_bsdf(self.wo, c.wi)) };
        let attenuation = scene.transmittance(&Ray::new(p, c.wi, c.tmin, c.tmax));
        c.contribution * attenuation * (self.weight_sum * mis)
    }

    fn add(&mut self, wi: Vec3, li: Spectrum, density: f32, delta: bool, tmin: f32, tmax: f32, rng: &mut Rng) {
        if !(density > 0.0) || is_black(li) || !self.se.may_scatter(wi) {
            return;
        }
        let contribution = li * self.se.evaluate_bsdf(self.wo, wi);
        let target = intensity(contribution);
        if !(target > 0.0) {
            return;
        }
        let weight = target / density;
        if !weight.is_finite() {
            return;
        }
        self.weight_sum += weight;
        if rng.f32() * self.weight_sum < weight {
            self.chosen = Some(Candidate { contribution: contribution / target, wi, density, delta, tmin, tmax });
        }
    }
}

/// Weight of the sun light a path finds at `hit` by itself, against connecting the previous
/// vertex to a sampled sun point (see `connect_sun_point`).
fn sun_weight(scene: &Scene, light: u32, hit: &Hit, scatter: Scatter) -> f32 {
    let Some((prev, pdf)) = scatter else {
        return 1.0;
    };
    let connection = scene.sun_point_density(light, hit);
    if connection <= 0.0 {
        return 1.0;
    }
    let d = hit.p - prev;
    let dist2 = d.length_squared();
    if dist2 <= 0.0 {
        return 1.0;
    }
    // pdf of the path's own sampling, converted to a density over the surface
    let path = pdf * hit.ng.dot(d).abs() / (dist2 * dist2.sqrt());
    mis_factor(path, connection)
}

/// Radiance scattered towards `wo` from a single sampled direction `wi` carrying radiance
/// `li`, `density` being the density w.r.t. solid angle the direction was sampled with.
fn shade_sample(
    se: &ScatteringEvent,
    p: Vec3,
    wo: Vec3,
    scene: &Scene,
    wi: Vec3,
    li: Spectrum,
    density: f32,
    delta: bool,
    (tmin, tmax): (f32, f32),
) -> Spectrum {
    if !(density > 0.0) || is_black(li) || !se.may_scatter(wi) {
        return Spectrum::ZERO;
    }
    let f = se.evaluate_bsdf(wo, wi);
    if is_black(f) {
        return Spectrum::ZERO;
    }
    let attenuation = scene.transmittance(&Ray::new(p, wi, tmin, tmax));
    if is_black(attenuation) {
        return Spectrum::ZERO;
    }
    // the continuation of the path may find the same light, weight one against the other
    let mis = if delta { 1.0 } else { mis_factor(density, se.pdf_bsdf(wo, wi)) };
    li * f * attenuation * (mis / density)
}

/// Direct illumination at a surface point from all the lights of the scene. One shadow ray
/// per sun, two for the sky, one for the punctual lights and one for the emissive surfaces.
pub fn evaluate_direct(
    se: &ScatteringEvent,
    hit: &Hit,
    wo: Vec3,
    scatter: Scatter,
    scene: &Scene,
    rng: &mut Rng,
) -> Spectrum {
    let p = hit.p;
    let mut radiance = Spectrum::ZERO;
    // one reservoir for the punctual lights, one for the emissive surfaces
    let mut reservoirs = [Reservoir::new(se, wo), Reservoir::new(se, wo)];

    let mut offer = |index: u32, rng: &mut Rng| {
        let scale = scene.light_scale(index);
        match &scene.lights[index as usize] {
            Light::Sky(sky) => {
                // One sample of the sky itself and one aimed at the portals, both weighted by
                // the density of the pair. Each has its own shadow ray: inside a room the
                // first one is almost always blocked.
                let range = (RAY_EPSILON, f32::MAX);
                if let Some((wi, li, _)) = sky.sample(rng.pair()) {
                    let density = scale * scene.sky_sampling_pdf(sky, p, wi);
                    radiance += shade_sample(se, p, wo, scene, wi, li, density, false, range);
                }
                if scene.has_portals() {
                    if let Some(wi) = scene.sample_portal(p, rng.f32(), rng.pair()) {
                        let density = scale * scene.sky_sampling_pdf(sky, p, wi);
                        radiance += shade_sample(se, p, wo, scene, wi, sky.evaluate(wi), density, false, range);
                    }
                }
            }
            // A sun is far brighter than anything else and is either fully visible or hidden.
            light @ Light::Distant { .. } => {
                if let Some(ls) = light.sample_l(scene, p, rng.pair()) {
                    let direct = shade_sample(se, p, wo, scene, ls.wi, ls.li, scale * ls.pdf, true, (ls.tmin, ls.tmax));
                    if !is_black(direct) {
                        let weight = if scene.has_sun_portals() { sun_weight(scene, index, hit, scatter) } else { 1.0 };
                        radiance += direct * weight;
                    }
                }
            }
            light => {
                let reservoir = &mut reservoirs[if light.is_delta() { 0 } else { 1 }];
                if let Some(ls) = light.sample_l(scene, p, rng.pair()) {
                    reservoir.add(ls.wi, ls.li, scale * ls.pdf, light.is_delta(), ls.tmin, ls.tmax, rng);
                }
            }
        }
    };

    for group in scene.light_groups() {
        if group.enumerate {
            for &index in &group.ids {
                offer(index, rng);
            }
        } else {
            for _ in 0..LIGHT_CANDIDATES {
                if let Some((k, _)) = group.distribution.sample_discrete(rng.f32()) {
                    offer(group.ids[k], rng);
                }
            }
        }
    }

    for reservoir in reservoirs {
        radiance += reservoir.resolve(p, scene);
    }
    radiance
}

/// Light bounced once off a point lit by the sun, towards a vertex of the path. `min_alpha`
/// is the blur accumulated by the path so far.
fn connect_sun_point(
    se: &ScatteringEvent,
    hit: &Hit,
    wo: Vec3,
    sun: &SunPoint,
    scene: &Scene,
    min_alpha: f32,
    filter_glossy: f32,
) -> Spectrum {
    let d = sun.hit.p - hit.p;
    let dist2 = d.length_squared();
    if dist2 < 1e-10 {
        return Spectrum::ZERO;
    }
    let dist = dist2.sqrt();
    let w = d / dist;
    let cos_at_sun_point = sun.hit.ng.dot(w).abs();
    if cos_at_sun_point <= 0.0 || !se.may_scatter(w) {
        return Spectrum::ZERO;
    }
    let f = se.evaluate_bsdf(wo, w);
    if is_black(f) {
        return Spectrum::ZERO;
    }
    // the sun point is shaded the way the path would shade it, had it gone there by itself
    let pdf = se.pdf_bsdf(wo, w);
    let min_alpha = min_alpha.max(lobe_width(filter_glossy, pdf));
    let reflected = scene.scattering_event(&sun.hit, -w, min_alpha).evaluate_bsdf(-w, -sun.dir);
    if is_black(reflected) {
        return Spectrum::ZERO;
    }

    // the path could also get there by itself and find the sun, see `sun_weight`
    let path = pdf * cos_at_sun_point / dist2;
    let weight = mis_factor(sun.density, path);

    let attenuation = scene.transmittance(&Ray::new(hit.p, w, RAY_EPSILON, dist - RAY_EPSILON));
    f * reflected * sun.irradiance * attenuation * (weight * cos_at_sun_point / (dist2 * sun.density))
}

impl PathTracing {
    /// Radiance arriving along a camera ray.
    pub fn li(&self, ray: &Ray, scene: &Scene, rng: &mut Rng) -> Spectrum {
        let mut l = Spectrum::ZERO;
        let mut throughput = Spectrum::ONE;

        // `None` for camera rays and after purely specular chains, for which sampling the
        // lights is not an alternative to finding them.
        let mut scatter: Scatter = None;
        // lower bound of the width of the specular lobes, see `filter_glossy`
        let mut min_alpha = 0.0f32;

        let sun_point = if scene.has_sun_portals() {
            scene.sample_sun_point(rng.f32(), rng.f32(), rng.pair())
        } else {
            None
        };

        let mut bounces = 0;
        let mut r = *ray;
        loop {
            // get the intersection between the ray and the scene
            let Some(hit) = scene.intersect(&r) else {
                let le = scene.le(&r);
                if !is_black(le) {
                    let weight = match scatter {
                        Some((p, pdf)) => mis_factor(pdf, scene.sky_density(p, r.dir)),
                        None => 1.0,
                    };
                    l += throughput * le * weight;
                }
                break;
            };
            let wo = -r.dir;

            // emission of the surface the path landed on
            let le = scene.emission(&hit, wo);
            if !is_black(le) {
                let weight = match scatter {
                    Some((p, pdf)) => mis_factor(pdf, scene.area_light_density(&hit, p, r.dir)),
                    None => 1.0,
                };
                l += throughput * le * weight;
            }

            // This introduces bias. 'max_recursive_depth' could be set large to reduce it.
            if bounces >= self.max_recursive_depth {
                break;
            }

            let se = scene.scattering_event(&hit, wo, min_alpha);
            if se.is_empty() {
                break;
            }

            // evaluate the lights
            l += throughput * evaluate_direct(&se, &hit, wo, scatter, scene, rng);

            // the sun bounced once, which a path of one more vertex would find by itself
            if let Some(sun) = &sun_point {
                if bounces + 1 < self.max_recursive_depth {
                    l += throughput
                        * connect_sun_point(&se, &hit, wo, sun, scene, min_alpha, self.filter_glossy);
                }
            }

            // sample the next direction using the bsdf
            let bs = rng.pair();
            let Some(sample) = se.sample_bsdf(wo, bs, rng) else {
                break;
            };
            if is_black(sample.f) || sample.pdf == 0.0 {
                break;
            }

            // update path weight
            throughput *= sample.f / sample.pdf;
            if intensity(throughput) == 0.0 {
                break;
            }

            // Going straight through thin glass does not change what the path is looking at:
            // the vertex before it stays the reference for weighting what is found next.
            if !sample.delta {
                scatter = Some((hit.p, sample.pdf));
                min_alpha = min_alpha.max(lobe_width(self.filter_glossy, sample.pdf));
            }
            r = Ray::new(hit.p, sample.wi, RAY_EPSILON, f32::MAX);

            // Russian roulette
            if bounces > 3 && throughput.max_element() < 0.1 {
                let terminate_probability = (1.0 - throughput.max_element()).max(0.05);
                if rng.f32() < terminate_probability {
                    break;
                }
                throughput /= 1.0 - terminate_probability;
            }

            bounces += 1;
        }

        l
    }
}

/// Ambient occlusion (`src/integrator/ao.cpp`): white, times the fraction of the hemisphere
/// around the first surface seen that is open within `max_distance`, cosine weighted.
pub struct AmbientOcclusion {
    /// Geometry farther than this from a point does not darken it, in scene units.
    pub max_distance: f32,
}

impl AmbientOcclusion {
    /// Thin transmissive surfaces the camera ray may cross before finding an opaque one.
    const MAX_CROSSINGS: u32 = 32;

    /// Occlusion seen along a camera ray, 1 where the ray leaves the scene.
    pub fn li(&self, ray: &Ray, scene: &Scene, rng: &mut Rng) -> Spectrum {
        // Like `Scene::first_opaque_hit`, but a ray that leaves the scene through glass keeps
        // the attenuation of that glass instead of being lost.
        let mut attenuation = Spectrum::ONE;
        let mut r = *ray;
        let mut crossings = 0;
        let hit = loop {
            let Some(hit) = scene.intersect(&r) else {
                return attenuation;
            };
            // A partially covering surface is there with probability its coverage: either it
            // is shaded like any other or the ray goes on unchanged.
            let material = &scene.materials[hit.material as usize];
            let coverage = material.coverage(hit.uv);
            if coverage >= 1.0 || rng.f32() < coverage {
                match material.glass_transmittance(hit.uv) {
                    None => break hit,
                    Some(tr) => attenuation *= tr,
                }
            }
            crossings += 1;
            if is_black(attenuation) || crossings >= Self::MAX_CROSSINGS {
                return Spectrum::ZERO;
            }
            r.tmin = hit.t + hit.t.abs() * 1e-5 + 1e-5;
        };

        // shading normal on the side the ray came from; normal maps are left out, a clay
        // render shows the geometry
        let n = if hit.ng.dot(r.dir) > 0.0 { -hit.n } else { hit.n };
        let (u, v) = rng.pair();
        let local = cos_sample_hemisphere(u, v);
        if local.y <= 0.0 {
            return Spectrum::ZERO;
        }
        let (s, t) = coordinate_system(n);
        let wi = s * local.x + n * local.y + t * local.z;

        // the cosine and the pdf of the cosine weighted sample cancel out
        attenuation * scene.transmittance(&Ray::new(hit.p, wi, RAY_EPSILON, self.max_distance))
    }
}
