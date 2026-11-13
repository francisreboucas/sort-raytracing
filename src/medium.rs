// This file is a part of SORT-rs, a Rust port of SORT (Simple Open Ray Tracing).
// Original work Copyright (c) 2011-2023 by Jiayin Cao, GPLv3.

//! Participating media: what light travels through absorbs and scatters it, as fog, haze,
//! smoke, tinted glass and water do.
//!
//! Ported from `src/medium`: the absorption medium and the homogeneous medium are one type
//! here (a medium that does not scatter is SORT's `AbsorptionMedium`), with the
//! Henyey-Greenstein phase function of `src/medium/phasefunction.h`. The heterogeneous medium
//! is not ported, it needs volume data glTF does not carry.
//!
//! SORT attaches a medium to any mesh through a volume shader and keeps a stack of the media
//! a ray is in. Here a medium is found in two places, which is what buildings need:
//!
//! - `Fog` fills a box, or a horizontal layer of unbounded extent, with a medium: haze over a
//!   site, the dust that shows the sun coming through a window. Being a box, a ray knows what
//!   it crosses without looking for a boundary, wherever it starts.
//! - `Material::medium` fills the inside of a solid transmissive object: thick tinted glass,
//!   the water of a pool, a block of resin.

use crate::math::*;
use crate::rng::Rng;

/// The Henyey-Greenstein phase function. `g` is the mean cosine of the scattering angle: 0
/// scatters evenly in all directions, towards 1 mostly forward (fog and haze are around 0.8),
/// towards -1 mostly backward.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HenyeyGreenstein {
    pub g: f32,
}

impl HenyeyGreenstein {
    pub fn new(g: f32) -> Self {
        Self { g: g.clamp(-0.99, 0.99) }
    }

    /// Value for the cosine of the angle between `wo` and `wi`, both pointing away from the
    /// point. It integrates to one over the sphere, so it is also the pdf of `sample`.
    #[inline]
    pub fn f(&self, cos_wo_wi: f32) -> f32 {
        let g = self.g;
        let denom = 1.0 + g * g + 2.0 * g * cos_wo_wi;
        (1.0 - g * g) / (4.0 * PI * denom * denom.max(0.0).sqrt())
    }

    /// Sample an incident direction for the outgoing direction `wo`.
    pub fn sample(&self, wo: Vec3, u: (f32, f32)) -> Vec3 {
        let g = self.g;
        // cosine of the angle to the direction light keeps going in, `-wo`
        let cos_theta = if g.abs() < 1e-3 {
            1.0 - 2.0 * u.0
        } else {
            let term = (1.0 - g * g) / (1.0 - g + 2.0 * g * u.0);
            ((1.0 + g * g - term * term) / (2.0 * g)).clamp(-1.0, 1.0)
        };
        let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
        let phi = TWO_PI * u.1;
        let forward = -wo;
        let (s, t) = coordinate_system(forward);
        (s * phi.cos() + t * phi.sin()) * sin_theta + forward * cos_theta
    }
}

/// A homogeneous medium. Coefficients are per unit of length of the scene (meters): light
/// that travels a distance `d` keeps `exp(-(sigma_a + sigma_s) d)` of itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Medium {
    /// Absorption coefficient: light that is lost.
    pub sigma_a: Spectrum,
    /// Scattering coefficient: light that changes direction. Black for a medium that only
    /// absorbs, like clear tinted glass.
    pub sigma_s: Spectrum,
    pub phase: HenyeyGreenstein,
}

/// What happens to a path along a stretch of medium.
pub enum MediumSample {
    /// The path scatters after distance `t`. `weight` multiplies its throughput.
    Scatter { t: f32, weight: Spectrum },
    /// The path gets to the end of the stretch. `weight` multiplies its throughput.
    Through { weight: Spectrum },
}

impl Medium {
    /// A medium that only absorbs: light keeps `color` of itself after travelling `distance`
    /// (glTF `attenuationColor` and `attenuationDistance`).
    pub fn absorbing(color: Spectrum, distance: f32) -> Self {
        let sigma_a = color.map(|c| -c.clamp(1e-6, 1.0).ln() / distance.max(1e-6));
        Self { sigma_a, sigma_s: Spectrum::ZERO, phase: HenyeyGreenstein::new(0.0) }
    }

    /// A medium that removes `density` of the light per unit of length (for a small length),
    /// of which the fraction `albedo` is scattered and the rest absorbed. White fog and haze
    /// have an albedo close to one, smoke a lower one.
    pub fn scattering(density: f32, albedo: Spectrum, g: f32) -> Self {
        let albedo = albedo.clamp(Spectrum::ZERO, Spectrum::ONE);
        let density = density.max(0.0);
        Self { sigma_a: (Spectrum::ONE - albedo) * density, sigma_s: albedo * density, phase: HenyeyGreenstein::new(g) }
    }

    #[inline]
    pub fn sigma_t(&self) -> Spectrum {
        self.sigma_a + self.sigma_s
    }

    /// Whether the medium changes anything at all.
    #[inline]
    pub fn is_empty(&self) -> bool {
        is_black(self.sigma_t())
    }

    /// Fraction of light left after travelling `distance`, which may be infinite.
    #[inline]
    pub fn transmittance(&self, distance: f32) -> Spectrum {
        // a channel the medium does not touch stays at one, whatever the distance
        self.sigma_t().map(|s| if s > 0.0 { (-s * distance).exp() } else { 1.0 })
    }

    /// Decide whether a path scatters within `distance` (which may be infinite) or gets to
    /// the end of it, as `HomogeneousMedium::Sample` does: the distance to the next collision
    /// is drawn for one color channel, picked at random, and weighted for all three.
    pub fn sample(&self, distance: f32, rng: &mut Rng) -> MediumSample {
        // nothing to sample without scattering, absorption is just a factor
        if is_black(self.sigma_s) {
            return MediumSample::Through { weight: self.transmittance(distance) };
        }

        let sigma_t = self.sigma_t();
        let channel = ((rng.f32() * 3.0) as usize).min(2);
        let s = sigma_t[channel];
        let t = if s > 0.0 { -(1.0 - rng.f32()).max(f32::MIN_POSITIVE).ln() / s } else { f32::INFINITY };

        if t < distance {
            let tr = self.transmittance(t);
            // density of scattering at `t`, averaged over the channel that could be picked
            let pdf = (sigma_t * tr).element_sum() / 3.0;
            let weight = if pdf > 0.0 { tr * self.sigma_s / pdf } else { Spectrum::ZERO };
            MediumSample::Scatter { t, weight }
        } else {
            let tr = self.transmittance(distance);
            // probability of getting through, averaged the same way
            let pdf = tr.element_sum() / 3.0;
            MediumSample::Through { weight: if pdf > 0.0 { tr / pdf } else { Spectrum::ZERO } }
        }
    }
}

/// A medium filling an axis-aligned box. The box may be unbounded: a layer of haze over the
/// ground has infinite `x` and `z` extents.
#[derive(Clone, Copy, Debug)]
pub struct Fog {
    pub medium: Medium,
    pub min: Vec3,
    pub max: Vec3,
}

impl Fog {
    /// The part of a ray that is inside the box, as a range of its parameter.
    pub fn interval(&self, ray: &Ray) -> Option<(f32, f32)> {
        let (mut t0, mut t1) = (ray.tmin.max(0.0), ray.tmax);
        for axis in 0..3 {
            let (o, d) = (ray.ori[axis], ray.dir[axis]);
            let (lo, hi) = (self.min[axis], self.max[axis]);
            if d == 0.0 {
                if o < lo || o > hi {
                    return None;
                }
                continue;
            }
            // an infinite bound gives an infinite distance of the right sign
            let (a, b) = ((lo - o) / d, (hi - o) / d);
            let (near, far) = if a < b { (a, b) } else { (b, a) };
            // a NaN (the origin on an infinite bound) leaves the range as it is
            t0 = t0.max(near);
            t1 = t1.min(far);
        }
        (t0 < t1).then_some((t0, t1))
    }

    /// Fraction of light the fog lets through along a ray.
    pub fn transmittance(&self, ray: &Ray) -> Spectrum {
        match self.interval(ray) {
            Some((t0, t1)) => self.medium.transmittance(t1 - t0),
            None => Spectrum::ONE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn henyey_greenstein_is_normalized_and_sampled_by_its_pdf() {
        let mut rng = Rng::new(7, 3);
        let wo = Vec3::new(0.3, 0.8, -0.5).normalize();
        for g in [-0.7, 0.0, 0.4, 0.9] {
            let phase = HenyeyGreenstein::new(g);
            let n = 200_000;
            // integral over the sphere, and the mean cosine to the forward direction, both
            // from uniform directions and from the phase function's own sampling
            let (mut integral, mut mean_uniform, mut mean_sampled) = (0.0f64, 0.0f64, 0.0f64);
            for _ in 0..n {
                let (u, v) = rng.pair();
                let wi = uniform_sample_sphere(u, v);
                let f = phase.f(wo.dot(wi)) as f64 / uniform_sphere_pdf() as f64;
                integral += f;
                mean_uniform += f * (-wo).dot(wi) as f64;
                let wi = phase.sample(wo, rng.pair());
                assert!((wi.length() - 1.0).abs() < 1e-3);
                mean_sampled += (-wo).dot(wi) as f64;
            }
            let (integral, mean_uniform, mean_sampled) =
                (integral / n as f64, mean_uniform / n as f64, mean_sampled / n as f64);
            assert!((integral - 1.0).abs() < 0.02, "g={g}: integral {integral}");
            // the mean cosine of Henyey-Greenstein is g
            assert!((mean_uniform - g as f64).abs() < 0.02, "g={g}: mean cosine {mean_uniform}");
            assert!((mean_sampled - g as f64).abs() < 0.01, "g={g}: sampled mean cosine {mean_sampled}");
        }
    }

    #[test]
    fn distance_sampling_is_unbiased() {
        let medium = Medium {
            sigma_a: Spectrum::new(0.1, 0.0, 0.3),
            sigma_s: Spectrum::new(0.5, 1.5, 0.0),
            phase: HenyeyGreenstein::new(0.0),
        };
        let mut rng = Rng::new(11, 5);
        for distance in [0.5, 2.0, f32::INFINITY] {
            let n = 400_000;
            let (mut through, mut scattered) = (Spectrum::ZERO, Spectrum::ZERO);
            for _ in 0..n {
                match medium.sample(distance, &mut rng) {
                    MediumSample::Through { weight } => through += weight,
                    MediumSample::Scatter { t, weight } => {
                        assert!(t < distance);
                        scattered += weight;
                    }
                }
            }
            let (through, scattered) = (through / n as f32, scattered / n as f32);
            let tr = medium.transmittance(distance);
            // light scattered within the distance: sigma_s / sigma_t of what does not get through
            let expected = (medium.sigma_s / medium.sigma_t()) * (Spectrum::ONE - tr);
            assert!((through - tr).abs().max_element() < 0.01, "{distance}: through {through} vs {tr}");
            assert!(
                (scattered - expected).abs().max_element() < 0.01,
                "{distance}: scattered {scattered} vs {expected}"
            );
        }
    }

    #[test]
    fn absorbing_medium_matches_its_color_at_its_distance() {
        let color = Spectrum::new(0.2, 0.7, 1.0);
        let medium = Medium::absorbing(color, 0.4);
        assert!((medium.transmittance(0.4) - color).abs().max_element() < 1e-4);
        assert!((medium.transmittance(0.8) - color * color).abs().max_element() < 1e-4);
        assert_eq!(medium.transmittance(f32::INFINITY), Spectrum::new(0.0, 0.0, 1.0));
        let mut rng = Rng::new(1, 1);
        assert!(matches!(medium.sample(1.0, &mut rng), MediumSample::Through { .. }));
    }

    #[test]
    fn fog_interval_handles_boxes_and_unbounded_layers() {
        let medium = Medium::scattering(1.0, Spectrum::ONE, 0.0);
        let fog = Fog { medium, min: Vec3::new(-1.0, 0.0, -1.0), max: Vec3::new(1.0, 2.0, 1.0) };
        let through = Ray::new(Vec3::new(-5.0, 1.0, 0.0), Vec3::X, 0.0, f32::MAX);
        assert_eq!(fog.interval(&through), Some((4.0, 6.0)));
        let inside = Ray::new(Vec3::new(0.0, 1.0, 0.0), Vec3::X, 0.0, 0.5);
        assert_eq!(fog.interval(&inside), Some((0.0, 0.5)));
        let beside = Ray::new(Vec3::new(-5.0, 3.0, 0.0), Vec3::X, 0.0, f32::MAX);
        assert_eq!(fog.interval(&beside), None);
        assert_eq!(fog.transmittance(&beside), Spectrum::ONE);

        let inf = f32::INFINITY;
        let layer = Fog { medium, min: Vec3::new(-inf, -inf, -inf), max: Vec3::new(inf, 2.0, inf) };
        let down = Ray::new(Vec3::new(0.0, 5.0, 0.0), -Vec3::Y, 0.0, 4.0);
        assert_eq!(layer.interval(&down), Some((3.0, 4.0)));
        let level = Ray::new(Vec3::new(0.0, 1.0, 0.0), Vec3::X, 0.0, f32::MAX);
        assert_eq!(layer.interval(&level), Some((0.0, f32::MAX)));
        assert_eq!(layer.transmittance(&level), Spectrum::ZERO);
        let above = Ray::new(Vec3::new(0.0, 3.0, 0.0), Vec3::X, 0.0, f32::MAX);
        assert_eq!(layer.interval(&above), None);
    }
}
