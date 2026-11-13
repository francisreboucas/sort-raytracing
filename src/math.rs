// This file is a part of SORT-rs, a Rust port of SORT (Simple Open Ray Tracing).
// Original work Copyright (c) 2011-2023 by Jiayin Cao, GPLv3.

//! Math utilities: rays, bounding boxes, shading-space helpers and sampling routines.
//!
//! Like SORT, the local shading frame is Y-up: `cos_theta(v) == v.y`.

pub use glam::{Mat4, Vec2, Vec3, Vec4};

pub const PI: f32 = std::f32::consts::PI;
pub const TWO_PI: f32 = 2.0 * PI;
pub const INV_PI: f32 = 1.0 / PI;
pub const INV_TWOPI: f32 = 1.0 / TWO_PI;

/// Offset of the origin of secondary rays, to avoid hitting the surface they start from.
pub const RAY_EPSILON: f32 = 1e-4;

/// RGB spectrum, as in SORT's `RGBSpectrum`.
pub type Spectrum = Vec3;

/// Luminance of an RGB spectrum (`Spectrum::GetIntensity`).
#[inline]
pub fn intensity(c: Spectrum) -> f32 {
    0.212671 * c.x + 0.715160 * c.y + 0.072169 * c.z
}

#[inline]
pub fn is_black(c: Spectrum) -> bool {
    c.x == 0.0 && c.y == 0.0 && c.z == 0.0
}

#[inline]
pub fn sqr(x: f32) -> f32 {
    x * x
}

#[inline]
pub fn saturate(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[inline]
pub fn lerp3(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    a + (b - a) * t
}

/// A ray with the precomputed data of `Ray::Prepare` used by the watertight triangle test.
#[derive(Clone, Copy, Debug)]
pub struct Ray {
    pub ori: Vec3,
    pub dir: Vec3,
    pub tmin: f32,
    pub tmax: f32,
    pub inv_dir: Vec3,
    pub local: [usize; 3],
    pub scale: Vec3,
}

impl Ray {
    pub fn new(ori: Vec3, dir: Vec3, tmin: f32, tmax: f32) -> Self {
        let a = dir.abs();
        let ly = if a.x > a.y && a.x > a.z {
            0
        } else if a.y > a.z {
            1
        } else {
            2
        };
        let lz = (ly + 1) % 3;
        let lx = (lz + 1) % 3;
        let d = Vec3::new(dir[lx], dir[ly], dir[lz]);
        Self {
            ori,
            dir,
            tmin,
            tmax,
            inv_dir: dir.recip(),
            local: [lx, ly, lz],
            scale: Vec3::new(-d.x / d.y, 1.0 / d.y, -d.z / d.y),
        }
    }

    #[inline]
    pub fn at(&self, t: f32) -> Vec3 {
        self.ori + self.dir * t
    }
}

/// Axis aligned bounding box.
#[derive(Clone, Copy, Debug)]
pub struct BBox {
    pub min: Vec3,
    pub max: Vec3,
}

impl Default for BBox {
    fn default() -> Self {
        Self { min: Vec3::splat(f32::MAX), max: Vec3::splat(-f32::MAX) }
    }
}

impl BBox {
    #[inline]
    pub fn union_point(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    #[inline]
    pub fn union(&mut self, b: &BBox) {
        self.min = self.min.min(b.min);
        self.max = self.max.max(b.max);
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x
    }

    #[inline]
    pub fn half_surface_area(&self) -> f32 {
        if self.is_empty() {
            return 0.0;
        }
        let d = self.max - self.min;
        d.x * d.y + d.y * d.z + d.z * d.x
    }

    #[inline]
    pub fn max_axis(&self) -> usize {
        let d = self.max - self.min;
        if d.x > d.y && d.x > d.z {
            0
        } else if d.y > d.z {
            1
        } else {
            2
        }
    }

    #[inline]
    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    /// Slab test. Returns the entry distance (clamped to `ray.tmin`) or a negative value on a miss.
    #[inline]
    pub fn intersect(&self, ray: &Ray, tmax: f32) -> f32 {
        let t0 = (self.min - ray.ori) * ray.inv_dir;
        let t1 = (self.max - ray.ori) * ray.inv_dir;
        let near = t0.min(t1).max_element().max(ray.tmin);
        let far = t0.max(t1).min_element().min(tmax);
        // written so that NaNs (ray lying in a slab plane) count as a miss
        if near <= far {
            near
        } else {
            -1.0
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Shading space helpers ( Y is up )
// ---------------------------------------------------------------------------------------------

#[inline]
pub fn cos_theta(v: Vec3) -> f32 {
    v.y
}
#[inline]
pub fn abs_cos_theta(v: Vec3) -> f32 {
    v.y.abs()
}
#[inline]
pub fn cos_theta2(v: Vec3) -> f32 {
    v.y * v.y
}
#[inline]
pub fn sin_theta2(v: Vec3) -> f32 {
    (1.0 - v.y * v.y).max(0.0)
}
#[inline]
pub fn tan_theta2(v: Vec3) -> f32 {
    sin_theta2(v) / cos_theta2(v)
}
#[inline]
pub fn cos_phi2(v: Vec3) -> f32 {
    let s = sin_theta2(v);
    if s == 0.0 {
        1.0
    } else {
        (v.x * v.x / s).min(1.0)
    }
}
#[inline]
pub fn same_hemisphere(a: Vec3, b: Vec3) -> bool {
    a.y * b.y > 0.0
}

/// Direction from spherical coordinates, theta measured from the Y axis.
#[inline]
pub fn spherical_vec(theta: f32, phi: f32) -> Vec3 {
    let (st, ct) = theta.sin_cos();
    let (sp, cp) = phi.sin_cos();
    Vec3::new(st * cp, ct, st * sp)
}

/// Mirror `v` around `n`.
#[inline]
pub fn reflect(v: Vec3, n: Vec3) -> Vec3 {
    2.0 * v.dot(n) * n - v
}

/// Refract `v` through a surface with normal `n`. `in_eta` is the index of refraction below the
/// surface, `ext_eta` the one above. Returns `None` on total internal reflection.
#[inline]
pub fn refract(v: Vec3, n: Vec3, in_eta: f32, ext_eta: f32) -> Option<Vec3> {
    let coso = v.dot(n);
    let eta = if coso > 0.0 { ext_eta / in_eta } else { in_eta / ext_eta };
    let t = 1.0 - eta * eta * (1.0 - coso * coso).max(0.0);
    if t <= 0.0 {
        return None;
    }
    let factor = if coso < 0.0 { -1.0 } else { 1.0 };
    Some(-eta * v + (eta * coso - factor * t.sqrt()) * n)
}

/// Build two vectors orthogonal to `n`.
#[inline]
pub fn coordinate_system(n: Vec3) -> (Vec3, Vec3) {
    let a = if n.x.abs() > n.y.abs() {
        Vec3::new(-n.z, 0.0, n.x) / (n.x * n.x + n.z * n.z).sqrt()
    } else {
        Vec3::new(0.0, n.z, -n.y) / (n.y * n.y + n.z * n.z).sqrt()
    };
    (a, n.cross(a))
}

// ---------------------------------------------------------------------------------------------
// Sampling routines ( core/samplemethod.h )
// ---------------------------------------------------------------------------------------------

#[inline]
pub fn uniform_sample_disk(u: f32, v: f32) -> (f32, f32) {
    let r = u.sqrt();
    let theta = TWO_PI * v;
    (r * theta.cos(), r * theta.sin())
}

#[inline]
pub fn cos_sample_hemisphere(u: f32, v: f32) -> Vec3 {
    let (x, z) = uniform_sample_disk(u, v);
    Vec3::new(x, (1.0 - x * x - z * z).max(0.0).sqrt(), z)
}

#[inline]
pub fn cos_hemisphere_pdf(v: Vec3) -> f32 {
    abs_cos_theta(v) * INV_PI
}

#[inline]
pub fn uniform_sample_sphere(u: f32, v: f32) -> Vec3 {
    let y = 1.0 - 2.0 * u;
    let r = (1.0 - y * y).max(0.0).sqrt();
    let phi = TWO_PI * v;
    Vec3::new(r * phi.cos(), y, r * phi.sin())
}

#[inline]
pub fn uniform_sphere_pdf() -> f32 {
    0.25 * INV_PI
}

/// Power heuristic used for multiple importance sampling (`MisFactor`).
#[inline]
pub fn mis_factor(f: f32, g: f32) -> f32 {
    (f * f) / (f * f + g * g)
}

/// Piecewise constant 1D distribution (`Distribution1D`).
#[derive(Clone, Debug, Default)]
pub struct Distribution1D {
    pub func: Vec<f32>,
    pub cdf: Vec<f32>,
    pub sum: f32,
}

impl Distribution1D {
    pub fn new(func: &[f32]) -> Self {
        let mut cdf = Vec::with_capacity(func.len() + 1);
        let mut acc = 0.0f64;
        cdf.push(0.0);
        for &f in func {
            acc += f.max(0.0) as f64;
            cdf.push(acc as f32);
        }
        let sum = acc as f32;
        if sum > 0.0 {
            for c in cdf.iter_mut() {
                *c /= sum;
            }
        }
        if let Some(last) = cdf.last_mut() {
            *last = 1.0;
        }
        Self { func: func.to_vec(), cdf, sum }
    }

    pub fn len(&self) -> usize {
        self.func.len()
    }

    pub fn is_empty(&self) -> bool {
        self.func.is_empty()
    }

    /// Probability of picking entry `i` with `sample_discrete`.
    pub fn discrete_pdf(&self, i: usize) -> f32 {
        if self.sum > 0.0 {
            self.func[i].max(0.0) / self.sum
        } else {
            0.0
        }
    }

    fn find(&self, u: f32) -> usize {
        let i = self.cdf.partition_point(|&c| c <= u);
        i.saturating_sub(1).min(self.func.len() - 1)
    }

    /// Pick an entry proportionally to its value. Returns the index and its probability.
    pub fn sample_discrete(&self, u: f32) -> Option<(usize, f32)> {
        if self.sum <= 0.0 || self.func.is_empty() {
            return None;
        }
        let i = self.find(u);
        Some((i, self.discrete_pdf(i)))
    }

    /// Sample a continuous position in [0,1). Returns the position, its pdf and the entry index.
    pub fn sample_continuous(&self, u: f32) -> (f32, f32, usize) {
        let n = self.func.len() as f32;
        if self.sum <= 0.0 {
            return (u, 1.0, ((u * n) as usize).min(self.func.len() - 1));
        }
        let i = self.find(u);
        let span = self.cdf[i + 1] - self.cdf[i];
        let du = if span > 0.0 { (u - self.cdf[i]) / span } else { 0.0 };
        ((i as f32 + du) / n, self.func[i].max(0.0) * n / self.sum, i)
    }
}
