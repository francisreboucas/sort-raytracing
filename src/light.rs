// This file is a part of SORT-rs, a Rust port of SORT (Simple Open Ray Tracing).
// Original work Copyright (c) 2011-2023 by Jiayin Cao, GPLv3.

//! Light sources, ported from `src/light`.
//!
//! Area lights are emissive triangles instead of SORT's analytic quad/disk/sphere shapes,
//! since that is how emitters come out of a mesh based scene description.

use crate::math::*;
use crate::scene::Scene;

/// Offset used by shadow rays towards lights. SORT uses 0.01, which in a scene modelled in
/// meters lets light leak through every wall closer than a centimeter, i.e. along all corners.
const LIGHT_DELTA: f32 = 1e-4;

/// An HDR environment map in latitude-longitude layout with its sampling distribution.
pub struct EnvMap {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<Spectrum>,
    marginal: Distribution1D,
    conditional: Vec<Distribution1D>,
}

impl EnvMap {
    pub fn new(width: usize, height: usize, pixels: Vec<Spectrum>) -> Self {
        assert_eq!(pixels.len(), width * height);
        let mut conditional = Vec::with_capacity(height);
        let mut row = vec![0.0f32; width];
        for y in 0..height {
            let sin_theta = (PI * (y as f32 + 0.5) / height as f32).sin();
            for (x, v) in row.iter_mut().enumerate() {
                *v = intensity(pixels[y * width + x]).max(0.0) * sin_theta;
            }
            conditional.push(Distribution1D::new(&row));
        }
        let marginal_func: Vec<f32> = conditional.iter().map(|c| c.sum / width as f32).collect();
        Self { width, height, pixels, marginal: Distribution1D::new(&marginal_func), conditional }
    }

    #[inline]
    fn dir_to_uv(dir: Vec3) -> (f32, f32) {
        // atan2 keeps its precision near the poles, unlike acos
        let theta = (dir.x * dir.x + dir.z * dir.z).sqrt().atan2(dir.y);
        let mut phi = dir.z.atan2(dir.x);
        if phi < 0.0 {
            phi += TWO_PI;
        }
        (phi * INV_TWOPI, theta * INV_PI)
    }

    #[inline]
    fn texel_index(&self, u: f32, v: f32) -> (usize, usize) {
        (
            ((u * self.width as f32) as usize).min(self.width - 1),
            ((v * self.height as f32) as usize).min(self.height - 1),
        )
    }

    pub fn evaluate(&self, dir: Vec3) -> Spectrum {
        let (u, v) = Self::dir_to_uv(dir);
        let (x, y) = self.texel_index(u, v);
        self.pixels[y * self.width + x]
    }

    /// Pdf w.r.t. solid angle of sampling `dir`.
    pub fn pdf(&self, dir: Vec3) -> f32 {
        let (u, v) = Self::dir_to_uv(dir);
        let sin_theta = (v * PI).sin();
        if sin_theta <= 0.0 || self.marginal.sum <= 0.0 {
            return 0.0;
        }
        let (x, y) = self.texel_index(u, v);
        let cond = &self.conditional[y];
        if cond.sum <= 0.0 {
            return 0.0;
        }
        let pdf_v = self.marginal.func[y] * self.height as f32 / self.marginal.sum;
        let pdf_u = cond.func[x] * self.width as f32 / cond.sum;
        pdf_u * pdf_v / (2.0 * PI * PI * sin_theta)
    }

    /// Importance sample a direction. Returns the direction, the radiance coming from it and
    /// its pdf w.r.t. solid angle.
    pub fn sample(&self, (u0, u1): (f32, f32)) -> Option<(Vec3, Spectrum, f32)> {
        if self.marginal.sum <= 0.0 {
            return None;
        }
        let (v, _, y) = self.marginal.sample_continuous(u0);
        let (u, _, _) = self.conditional[y].sample_continuous(u1);
        let dir = spherical_vec(v * PI, u * TWO_PI);
        // The radiance and the pdf are looked up from the direction itself: a sample right on
        // the border of a texel may map back to its neighbor, and pairing the radiance of one
        // texel with the pdf of the other gives fireflies around bright spots like the sun.
        let pdf = self.pdf(dir);
        (pdf > 0.0).then(|| (dir, self.evaluate(dir), pdf))
    }

    pub fn average(&self) -> Spectrum {
        self.pixels.iter().copied().sum::<Spectrum>() / self.pixels.len().max(1) as f32
    }
}

/// Sky light, either a constant color or an HDR environment map.
pub struct Sky {
    /// Radiance of a constant sky, or a multiplier of the environment map.
    pub intensity: Spectrum,
    pub env: Option<EnvMap>,
    /// Rotation of the environment map around the vertical axis, in radians.
    pub rotation: f32,
}

impl Sky {
    #[inline]
    fn rotate(&self, dir: Vec3, angle: f32) -> Vec3 {
        let (s, c) = angle.sin_cos();
        Vec3::new(c * dir.x - s * dir.z, dir.y, s * dir.x + c * dir.z)
    }

    pub fn evaluate(&self, dir: Vec3) -> Spectrum {
        match &self.env {
            Some(env) => env.evaluate(self.rotate(dir, -self.rotation)) * self.intensity,
            None => self.intensity,
        }
    }

    pub fn pdf(&self, dir: Vec3) -> f32 {
        match &self.env {
            Some(env) => env.pdf(self.rotate(dir, -self.rotation)),
            None => uniform_sphere_pdf(),
        }
    }

    /// Sample a direction. Returns the direction, the radiance coming from it and its pdf.
    pub fn sample(&self, u: (f32, f32)) -> Option<(Vec3, Spectrum, f32)> {
        match &self.env {
            Some(env) => env
                .sample(u)
                .map(|(dir, li, pdf)| (self.rotate(dir, self.rotation), li * self.intensity, pdf)),
            None => Some((uniform_sample_sphere(u.0, u.1), self.intensity, uniform_sphere_pdf())),
        }
    }
}

pub enum Light {
    /// Light coming from infinitely far away, like the sun. `dir` is the direction it travels.
    Distant { dir: Vec3, intensity: Spectrum },
    Point { pos: Vec3, intensity: Spectrum },
    Spot { pos: Vec3, dir: Vec3, cos_total_range: f32, cos_falloff_start: f32, intensity: Spectrum },
    /// An emissive triangle of the scene.
    Area { tri: u32 },
    Sky(Sky),
}

/// A direction sampled towards a light.
pub struct LightSample {
    /// Direction from the shaded point to the light.
    pub wi: Vec3,
    pub li: Spectrum,
    pub pdf: f32,
    /// Range of the shadow ray.
    pub tmin: f32,
    pub tmax: f32,
}

impl Light {
    /// Whether the light can only be reached through light sampling.
    #[inline]
    pub fn is_delta(&self) -> bool {
        matches!(self, Light::Distant { .. } | Light::Point { .. } | Light::Spot { .. })
    }

    /// Sample incident radiance at `p`.
    pub fn sample_l(&self, scene: &Scene, p: Vec3, u: (f32, f32)) -> Option<LightSample> {
        match self {
            Light::Distant { dir, intensity } => {
                Some(LightSample { wi: -*dir, li: *intensity, pdf: 1.0, tmin: LIGHT_DELTA, tmax: f32::MAX })
            }
            Light::Point { pos, intensity } => {
                let d = *pos - p;
                let sqr_len = d.length_squared();
                let len = sqr_len.sqrt();
                (len > 0.0).then(|| LightSample { wi: d / len, li: *intensity, pdf: sqr_len, tmin: LIGHT_DELTA, tmax: len })
            }
            Light::Spot { pos, dir, cos_total_range, cos_falloff_start, intensity } => {
                let d = *pos - p;
                let sqr_len = d.length_squared();
                let len = sqr_len.sqrt();
                if len <= 0.0 {
                    return None;
                }
                let wi = d / len;
                let falloff = saturate(wi.dot(-*dir));
                if falloff <= *cos_total_range {
                    return None;
                }
                let li = if falloff >= *cos_falloff_start {
                    *intensity
                } else {
                    let d = (falloff - cos_total_range) / (cos_falloff_start - cos_total_range);
                    *intensity * (d * d)
                };
                Some(LightSample { wi, li, pdf: sqr_len, tmin: LIGHT_DELTA, tmax: len })
            }
            Light::Area { tri } => {
                let [p0, p1, p2] = scene.triangle_positions(*tri);
                let su = u.0.sqrt();
                let (b0, b1) = (1.0 - su, u.1 * su);
                let b2 = 1.0 - b0 - b1;
                let ps = p0 * b0 + p1 * b1 + p2 * b2;

                let cross = (p1 - p0).cross(p2 - p0);
                let area = cross.length() * 0.5;
                let d = ps - p;
                let sqr_len = d.length_squared();
                let len = sqr_len.sqrt();
                if area <= 0.0 || len <= 0.0 {
                    return None;
                }
                let wi = d / len;
                let cos_at_light = -wi.dot(cross / (2.0 * area));
                let material = scene.triangle_material(*tri);
                if cos_at_light == 0.0 || (cos_at_light < 0.0 && !material.double_sided) {
                    return None;
                }
                let li = material.emission(scene.triangle_uv(*tri, b1, b2));
                Some(LightSample {
                    wi,
                    li,
                    pdf: sqr_len / (area * cos_at_light.abs()),
                    tmin: LIGHT_DELTA,
                    tmax: len - LIGHT_DELTA,
                })
            }
            Light::Sky(sky) => {
                let (wi, li, pdf) = sky.sample(u)?;
                Some(LightSample { wi, li, pdf, tmin: LIGHT_DELTA, tmax: f32::MAX })
            }
        }
    }

    /// Rough estimate of the emitted power, only used to choose which light to sample.
    pub fn power(&self, scene: &Scene, world_radius: f32) -> f32 {
        let disk = PI * world_radius * world_radius;
        match self {
            Light::Distant { intensity: i, .. } => intensity(*i) * disk,
            Light::Point { intensity: i, .. } => intensity(*i) * 4.0 * PI,
            Light::Spot { intensity: i, cos_total_range, cos_falloff_start, .. } => {
                intensity(*i) * TWO_PI * (1.0 - 0.5 * (cos_total_range + cos_falloff_start))
            }
            Light::Area { tri } => {
                let [p0, p1, p2] = scene.triangle_positions(*tri);
                let area = (p1 - p0).cross(p2 - p0).length() * 0.5;
                let [uv0, uv1, uv2] = scene.triangle_uvs(*tri);
                let emission = scene.triangle_material(*tri).emission((uv0 + uv1 + uv2) / 3.0);
                area * intensity(emission) * TWO_PI
            }
            Light::Sky(sky) => {
                let average = match &sky.env {
                    Some(env) => env.average() * sky.intensity,
                    None => sky.intensity,
                };
                intensity(average) * PI * disk
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    #[test]
    fn env_map_sampling_matches_pdf() {
        // a small map with a bright spot
        let (w, h) = (32, 16);
        let mut pixels = vec![Spectrum::splat(0.2); w * h];
        pixels[5 * w + 20] = Spectrum::splat(50.0);
        let env = EnvMap::new(w, h, pixels);

        let mut rng = Rng::new(3, 1);
        let n = 200_000;
        let (mut importance, mut uniform, mut pdf_integral) = (0.0f64, 0.0f64, 0.0f64);
        for _ in 0..n {
            let (dir, li, pdf) = env.sample(rng.pair()).unwrap();
            assert_eq!(env.pdf(dir), pdf);
            assert_eq!(env.evaluate(dir), li);
            importance += (intensity(li) / pdf) as f64;

            let (u, v) = rng.pair();
            let dir = uniform_sample_sphere(u, v);
            uniform += (intensity(env.evaluate(dir)) / uniform_sphere_pdf()) as f64;
            pdf_integral += (env.pdf(dir) / uniform_sphere_pdf()) as f64;
        }
        let (importance, uniform, pdf_integral) = (importance / n as f64, uniform / n as f64, pdf_integral / n as f64);
        assert!((importance - uniform).abs() < 0.05 * uniform, "{importance} vs {uniform}");
        assert!((pdf_integral - 1.0).abs() < 0.05, "pdf integrates to {pdf_integral}");
    }
}
