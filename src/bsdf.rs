// This file is a part of SORT-rs, a Rust port of SORT (Simple Open Ray Tracing).
// Original work Copyright (c) 2011-2023 by Jiayin Cao, GPLv3.

//! Scattering: microfacet distributions, Fresnel terms, BXDFs and the scattering event.
//!
//! Ported from `src/scatteringevent`. As in SORT every BXDF works in a Y-up local frame and
//! its value already includes the cosine term of the incident direction.

use crate::math::*;
use crate::rng::Rng;

// ---------------------------------------------------------------------------------------------
// Fresnel ( fresnel.h )
// ---------------------------------------------------------------------------------------------

#[inline]
pub fn schlick_weight(cos: f32) -> f32 {
    let w = saturate(1.0 - cos);
    let w2 = w * w;
    w2 * w2 * w
}

#[inline]
pub fn schlick_r0_from_eta(eta: f32) -> f32 {
    sqr((eta - 1.0) / (eta + 1.0))
}

/// Dielectric Fresnel. `cos_i` may be negative, meaning the ray leaves the surface from inside.
pub fn dielectric_fresnel(mut cos_i: f32, eta_i: f32, eta_t: f32) -> f32 {
    let entering = cos_i > 0.0;
    let (ei, et) = if entering { (eta_i, eta_t) } else { (eta_t, eta_i) };
    let sin_i = saturate(1.0 - sqr(cos_i)).sqrt();
    let sin_t = ei * sin_i / et;
    if sin_t >= 1.0 {
        return 1.0;
    }
    if !entering {
        cos_i = -cos_i;
    }
    let cos_t = (1.0 - sin_t * sin_t).sqrt();
    let (t0, t1, t2, t3) = (et * cos_i, ei * cos_t, ei * cos_i, et * cos_t);
    let r_parl = (t0 - t1) / (t0 + t1);
    let r_perp = (t2 - t3) / (t2 + t3);
    (r_parl * r_parl + r_perp * r_perp) * 0.5
}

pub fn conductor_fresnel(cos_i: f32, eta: Spectrum, k: Spectrum) -> Spectrum {
    let sq_cos = cos_i * cos_i;
    let t = 2.0 * eta * cos_i;
    let tmp_f = eta * eta + k * k;
    let tmp = tmp_f * sq_cos;
    let r_parl2 = (tmp - t + 1.0) / (tmp + t + 1.0);
    let r_perp2 = (tmp_f - t + sq_cos) / (tmp_f + t + sq_cos);
    (r_parl2 + r_perp2) * 0.5
}

#[derive(Clone, Copy, Debug)]
pub enum Fresnel {
    /// No Fresnel at all, a perfect reflector.
    No,
    Conductor { eta: Spectrum, k: Spectrum },
    Dielectric { eta_i: f32, eta_t: f32 },
    Schlick(Spectrum),
}

impl Fresnel {
    #[inline]
    pub fn evaluate(&self, cos_i: f32) -> Spectrum {
        match *self {
            Fresnel::No => Spectrum::ONE,
            Fresnel::Conductor { eta, k } => conductor_fresnel(cos_i, eta, k),
            Fresnel::Dielectric { eta_i, eta_t } => {
                Spectrum::splat(dielectric_fresnel(cos_i, eta_i, eta_t))
            }
            Fresnel::Schlick(f0) => f0 + schlick_weight(cos_i) * (Spectrum::ONE - f0),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Microfacet distributions ( microfacet.cpp )
// ---------------------------------------------------------------------------------------------

/// Anisotropic GGX (Trowbridge-Reitz) distribution.
#[derive(Clone, Copy, Debug)]
pub struct Ggx {
    alpha_u: f32,
    alpha_v: f32,
    alpha_u2: f32,
    alpha_v2: f32,
    alpha_uv: f32,
    alpha: f32,
}

impl Ggx {
    pub fn new(roughness_u: f32, roughness_v: f32) -> Self {
        // UE4 style roughness to alpha conversion, it keeps sharp reflection with low roughness.
        let convert = |r: f32| sqr(r.max(1e-3));
        let (alpha_u, alpha_v) = (convert(roughness_u), convert(roughness_v));
        Self {
            alpha_u,
            alpha_v,
            alpha_u2: alpha_u * alpha_u,
            alpha_v2: alpha_v * alpha_v,
            alpha_uv: alpha_u * alpha_v,
            alpha: alpha_v / alpha_u,
        }
    }

    pub fn d(&self, h: Vec3) -> f32 {
        let cos_theta_h_sq = cos_theta2(h);
        if cos_theta_h_sq <= 0.0 {
            return 0.0;
        }
        let beta = cos_theta_h_sq + sqr(h.x) / self.alpha_u2 + sqr(h.z) / self.alpha_v2;
        1.0 / (PI * self.alpha_uv * beta * beta)
    }

    /// Importance sample a micro-normal.
    pub fn sample(&self, (u, v): (f32, f32)) -> Vec3 {
        let (theta, phi);
        if self.alpha_u == self.alpha_v {
            theta = (self.alpha_u * (v / (1.0 - v)).sqrt()).atan();
            phi = TWO_PI * u;
        } else {
            const OFFSET: [f32; 5] = [0.0, 1.0, 1.0, 2.0, 2.0];
            let i = if v == 0.25 { 0 } else { ((v * 4.0) as usize).min(4) };
            phi = (self.alpha * (TWO_PI * v).tan()).atan() + OFFSET[i] * PI;
            let sin_phi_sq = sqr(phi.sin());
            let cos_phi_sq = 1.0 - sin_phi_sq;
            let beta = 1.0 / (cos_phi_sq / self.alpha_u2 + sin_phi_sq / self.alpha_v2);
            theta = (beta * u / (1.0 - u)).sqrt().atan();
        }
        spherical_vec(theta, phi)
    }

    pub fn g1(&self, v: Vec3) -> f32 {
        let tan_theta_sq = tan_theta2(v);
        if tan_theta_sq.is_infinite() || tan_theta_sq.is_nan() {
            return 0.0;
        }
        let cos_phi_sq = cos_phi2(v);
        let alpha2 = cos_phi_sq * self.alpha_u2 + (1.0 - cos_phi_sq) * self.alpha_v2;
        2.0 / (1.0 + (1.0 + alpha2 * tan_theta_sq).sqrt())
    }

    #[inline]
    pub fn g(&self, wo: Vec3, wi: Vec3) -> f32 {
        self.g1(wo) * self.g1(wi)
    }

    /// Pdf of sampling the micro-normal `wh`.
    #[inline]
    pub fn pdf(&self, wh: Vec3) -> f32 {
        self.d(wh) * abs_cos_theta(wh)
    }
}

/// The GTR1 distribution used by the clearcoat layer of the Disney BRDF.
#[derive(Clone, Copy, Debug)]
pub struct ClearcoatGgx {
    alpha2: f32,
}

impl ClearcoatGgx {
    pub fn new(roughness: f32) -> Self {
        let alpha = sqr(roughness.max(1e-3));
        Self { alpha2: alpha * alpha }
    }

    pub fn d(&self, h: Vec3) -> f32 {
        let cos = cos_theta(h);
        (self.alpha2 - 1.0) / (PI * self.alpha2.ln() * (1.0 + (self.alpha2 - 1.0) * sqr(cos)))
    }

    pub fn sample(&self, (u, v): (f32, f32)) -> Vec3 {
        let phi = TWO_PI * u;
        let cos = (((self.alpha2.ln() * v).exp() - 1.0) / (self.alpha2 - 1.0)).max(0.0).sqrt();
        spherical_vec(cos.min(1.0).acos(), phi)
    }

    pub fn g1(&self, v: Vec3) -> f32 {
        // SORT returns 0 when |cos| is exactly 1, which blacks out the coat at perfectly normal
        // incidence. That special case is dropped here, the formula itself is well defined.
        let tan_theta_sq = tan_theta2(v);
        if tan_theta_sq.is_infinite() || tan_theta_sq.is_nan() {
            return 0.0;
        }
        const ALPHA2: f32 = 0.25 * 0.25;
        1.0 / (1.0 + (1.0 + ALPHA2 * tan_theta_sq).sqrt())
    }

    #[inline]
    pub fn g(&self, wo: Vec3, wi: Vec3) -> f32 {
        self.g1(wo) * self.g1(wi)
    }

    #[inline]
    pub fn pdf(&self, wh: Vec3) -> f32 {
        self.d(wh) * abs_cos_theta(wh)
    }
}

// ---------------------------------------------------------------------------------------------
// Microfacet reflection / refraction
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct MicrofacetReflection {
    pub r: Spectrum,
    pub dist: Ggx,
    pub fresnel: Fresnel,
}

impl MicrofacetReflection {
    pub fn f(&self, wo: Vec3, wi: Vec3) -> Spectrum {
        if !same_hemisphere(wo, wi) || cos_theta(wo) <= 0.0 {
            return Spectrum::ZERO;
        }
        let n_o_v = abs_cos_theta(wo);
        if n_o_v == 0.0 {
            return Spectrum::ZERO;
        }
        let wh = (wi + wo).normalize_or_zero();
        let f = self.fresnel.evaluate(wo.dot(wh));
        self.r * f * (self.dist.d(wh) * self.dist.g(wo, wi) / (4.0 * n_o_v))
    }

    pub fn sample(&self, wo: Vec3, bs: (f32, f32)) -> Option<(Spectrum, Vec3, f32)> {
        let wh = self.dist.sample(bs);
        let wi = reflect(wo, wh);
        let pdf = self.pdf(wo, wi);
        if pdf <= 0.0 {
            return None;
        }
        Some((self.f(wo, wi), wi, pdf))
    }

    pub fn pdf(&self, wo: Vec3, wi: Vec3) -> f32 {
        if !same_hemisphere(wo, wi) || cos_theta(wo) <= 0.0 {
            return 0.0;
        }
        let h = (wo + wi).normalize_or_zero();
        let e_o_h = wo.dot(h).abs();
        if e_o_h == 0.0 {
            return 0.0;
        }
        self.dist.pdf(h) / (4.0 * e_o_h)
    }
}

/// Rough dielectric transmission ( Walter et al. 2007 ).
#[derive(Clone, Copy, Debug)]
pub struct MicrofacetRefraction {
    pub t: Spectrum,
    pub dist: Ggx,
    /// Index of refraction above the surface.
    pub eta_i: f32,
    /// Index of refraction below the surface.
    pub eta_t: f32,
}

impl MicrofacetRefraction {
    pub fn f(&self, wo: Vec3, wi: Vec3) -> Spectrum {
        if same_hemisphere(wi, wo) {
            return Spectrum::ZERO;
        }
        let n_o_v = cos_theta(wo);
        if n_o_v == 0.0 {
            return Spectrum::ZERO;
        }
        let eta = if n_o_v > 0.0 { self.eta_t / self.eta_i } else { self.eta_i / self.eta_t };
        let mut wh = (wo + wi * eta).normalize_or_zero();
        if wh.y < 0.0 {
            wh = -wh;
        }
        let s_v_o_h = wo.dot(wh);
        let s_i_o_h = wi.dot(wh);
        // Both directions have to be on the front side of the micro-facet they are on
        // ( the chi+ terms of Walter et al. ). SORT omits this check, which makes the BTDF
        // non-zero for directions its own importance sampling can never generate.
        if s_v_o_h * n_o_v <= 0.0 || s_i_o_h * cos_theta(wi) <= 0.0 {
            return Spectrum::ZERO;
        }
        let sqrt_denom = s_v_o_h + eta * s_i_o_h;
        if sqrt_denom == 0.0 {
            return Spectrum::ZERO;
        }
        let fresnel = dielectric_fresnel(wh.dot(wo), self.eta_i, self.eta_t);
        let t = eta / sqrt_denom;
        (1.0 - fresnel)
            * self.t
            * (self.dist.d(wh) * self.dist.g(wo, wi) * t * t * s_i_o_h * s_v_o_h / n_o_v).abs()
    }

    /// Sample a refracted direction only.
    pub fn sample_dir(&self, wo: Vec3, bs: (f32, f32)) -> Option<Vec3> {
        if cos_theta(wo) == 0.0 {
            return None;
        }
        let wh = self.dist.sample(bs);
        refract(wo, wh, self.eta_t, self.eta_i)
    }

    pub fn sample(&self, wo: Vec3, bs: (f32, f32)) -> Option<(Spectrum, Vec3, f32)> {
        let wi = self.sample_dir(wo, bs)?;
        let pdf = self.pdf(wo, wi);
        if pdf <= 0.0 {
            return None;
        }
        Some((self.f(wo, wi), wi, pdf))
    }

    pub fn pdf(&self, wo: Vec3, wi: Vec3) -> f32 {
        if same_hemisphere(wo, wi) {
            return 0.0;
        }
        let eta = if cos_theta(wo) > 0.0 { self.eta_t / self.eta_i } else { self.eta_i / self.eta_t };
        let mut wh = (wo + wi * eta).normalize_or_zero();
        if wh.y < 0.0 {
            wh = -wh;
        }
        if wo.dot(wh) * cos_theta(wo) <= 0.0 || wi.dot(wh) * cos_theta(wi) <= 0.0 {
            return 0.0;
        }
        let sqrt_denom = wo.dot(wh) + eta * wi.dot(wh);
        if sqrt_denom == 0.0 {
            return 0.0;
        }
        let dwh_dwi = eta * eta * wi.dot(wh).abs() / (sqrt_denom * sqrt_denom);
        self.dist.pdf(wh) * dwh_dwi
    }
}

// ---------------------------------------------------------------------------------------------
// Disney BRDF ( disney.cpp ), without the sub-surface scattering branch
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct Disney {
    pub basecolor: Spectrum,
    pub metallic: f32,
    pub specular: f32,
    pub specular_tint: f32,
    pub roughness: f32,
    pub anisotropic: f32,
    pub sheen: f32,
    pub sheen_tint: f32,
    pub clearcoat: f32,
    pub clearcoat_gloss: f32,
    pub spec_trans: f32,
    pub flatness: f32,
    pub diff_trans: f32,
    pub thin_surface: bool,
    /// Index of refraction below the surface. Hard coded to 1.5 in SORT.
    pub ior: f32,
}

impl Default for Disney {
    fn default() -> Self {
        Self {
            basecolor: Spectrum::splat(0.8),
            metallic: 0.0,
            specular: 1.0,
            specular_tint: 0.0,
            roughness: 0.5,
            anisotropic: 0.0,
            sheen: 0.0,
            sheen_tint: 0.0,
            clearcoat: 0.0,
            clearcoat_gloss: 1.0,
            spec_trans: 0.0,
            flatness: 0.0,
            diff_trans: 0.0,
            thin_surface: false,
            ior: 1.5,
        }
    }
}

const IOR_EX: f32 = 1.0;

/// 8.0 bears no physical meaning, it only raises the probability of picking the specular lobe,
/// which is by far the main source of fireflies.
#[inline]
fn specular_pdf_scale(roughness: f32) -> f32 {
    8.0 * (1.0 - roughness)
}

struct DisneyLobes {
    clearcoat: f32,
    specular_reflection: f32,
    specular_transmission: f32,
    diffuse_reflection: f32,
    diffuse_transmission: f32,
    total: f32,
}

impl Disney {
    #[inline]
    fn aspect(&self) -> f32 {
        (1.0 - self.anisotropic * 0.9).sqrt().sqrt()
    }

    #[inline]
    fn ggx(&self) -> Ggx {
        let aspect = self.aspect();
        Ggx::new(self.roughness / aspect, self.roughness * aspect)
    }

    /// Roughness scaled based on IOR for thin surfaces ( Burley 2015, Figure 15 ).
    #[inline]
    fn thin_ggx(&self) -> Ggx {
        let aspect = self.aspect();
        let rscaled = (0.65 * (self.ior / IOR_EX) - 0.35) * self.roughness;
        Ggx::new(sqr(rscaled) / aspect, sqr(rscaled) * aspect)
    }

    #[inline]
    fn clearcoat_ggx(&self) -> ClearcoatGgx {
        ClearcoatGgx::new(lerp(0.1, 0.001, self.clearcoat_gloss).sqrt())
    }

    #[inline]
    fn ctint(&self) -> Spectrum {
        let luminance = intensity(self.basecolor);
        if luminance > 0.0 {
            self.basecolor / luminance
        } else {
            Spectrum::ONE
        }
    }

    #[inline]
    fn cspec0(&self) -> Spectrum {
        let r0 = schlick_r0_from_eta(IOR_EX / self.ior);
        lerp3(
            self.specular * r0 * lerp3(Spectrum::ONE, self.ctint(), self.specular_tint),
            self.basecolor,
            self.metallic,
        )
    }

    fn refraction(&self, t: Spectrum, dist: Ggx) -> MicrofacetRefraction {
        MicrofacetRefraction { t, dist, eta_i: IOR_EX, eta_t: self.ior }
    }

    fn lobes(&self) -> DisneyLobes {
        let base = intensity(self.basecolor);
        let dielectric = base * (1.0 - self.metallic);
        let clearcoat = self.clearcoat * 0.04;
        let specular_reflection = intensity(self.cspec0()) * specular_pdf_scale(self.roughness);
        let specular_transmission = dielectric * self.spec_trans;
        let diffuse_reflection = dielectric
            * (1.0 - self.spec_trans)
            * if self.thin_surface { 1.0 - self.diff_trans } else { 1.0 };
        let diffuse_transmission =
            if self.thin_surface { dielectric * (1.0 - self.spec_trans) * self.diff_trans } else { 0.0 };
        DisneyLobes {
            clearcoat,
            specular_reflection,
            specular_transmission,
            diffuse_reflection,
            diffuse_transmission,
            total: clearcoat
                + specular_reflection
                + specular_transmission
                + diffuse_reflection
                + diffuse_transmission,
        }
    }

    pub fn f(&self, wo: Vec3, wi: Vec3) -> Spectrum {
        let diffuse_weight = (1.0 - self.metallic) * (1.0 - self.spec_trans);

        let wh = (wo + wi).normalize_or_zero();
        let h_o_o = wo.dot(wh);
        let h_o_o2_by_roughness = sqr(h_o_o) * self.roughness;
        let ctint = self.ctint();

        let mut ret = Spectrum::ZERO;
        let evaluate_reflection = cos_theta(wo) > 0.0 && cos_theta(wi) > 0.0;

        if diffuse_weight > 0.0 && evaluate_reflection {
            let n_o_o = cos_theta(wo);
            let n_o_i = cos_theta(wi);
            let clamped_n_o_i = saturate(n_o_i);
            let fo = schlick_weight(n_o_o);
            let fi = schlick_weight(n_o_i);

            // Extending the Disney BRDF to a BSDF with Integrated Subsurface Scattering, eq (4)
            let disney_diffuse = self.basecolor * (INV_PI * (1.0 - fo * 0.5) * (1.0 - fi * 0.5));
            if self.thin_surface {
                if self.flatness < 1.0 {
                    ret += diffuse_weight
                        * (1.0 - self.flatness)
                        * (1.0 - self.diff_trans)
                        * clamped_n_o_i
                        * disney_diffuse;
                }
                if self.flatness > 0.0 {
                    // Fake sub-surface scattering, based on the Hanrahan-Krueger BRDF
                    // approximation of the isotropic BSSRDF.
                    let fss90 = h_o_o2_by_roughness;
                    let fss = lerp(1.0, fss90, fo) * lerp(1.0, fss90, fi);
                    let fake_ss = self.basecolor
                        * (1.25 * (fss * (1.0 / (n_o_o + n_o_i) - 0.5) + 0.5) * INV_PI);
                    ret += diffuse_weight
                        * self.flatness
                        * (1.0 - self.diff_trans)
                        * clamped_n_o_i
                        * fake_ss;
                }
            } else {
                ret += diffuse_weight * clamped_n_o_i * disney_diffuse;
            }

            // Retro-reflection
            let rr = 2.0 * h_o_o2_by_roughness;
            let frr = self.basecolor * (INV_PI * rr * (fo + fi + fo * fi * (rr - 1.0)));
            ret += diffuse_weight * clamped_n_o_i * frr;

            // Not totally physically correct, but the sheen roughly compensates the energy lost
            // by the lack of microfacet inter-reflection.
            if self.sheen > 0.0 {
                let csheen = lerp3(Spectrum::ONE, ctint, self.sheen_tint);
                let fh = schlick_weight(h_o_o);
                ret += diffuse_weight * clamped_n_o_i * fh * self.sheen * csheen;
            }
        }

        // Specular reflection
        let ggx = self.ggx();
        let cspec0 = self.cspec0();
        if !is_black(cspec0) && evaluate_reflection {
            let mf = MicrofacetReflection {
                r: Spectrum::ONE,
                dist: ggx,
                fresnel: Fresnel::Schlick(cspec0),
            };
            ret += mf.f(wo, wi);
        }

        // Another layer of clear coat on top of everything below
        if self.clearcoat > 0.0 && evaluate_reflection {
            let cggx = self.clearcoat_ggx();
            let n_o_v = abs_cos_theta(wo);
            if n_o_v > 0.0 {
                let fresnel = 0.04 + schlick_weight(h_o_o) * (1.0 - 0.04);
                ret += Spectrum::splat(
                    self.clearcoat * cggx.d(wh) * fresnel * cggx.g(wo, wi) / (4.0 * n_o_v),
                );
            }
        }

        // Specular transmission
        if self.spec_trans > 0.0 {
            let mr = if self.thin_surface {
                self.refraction(self.basecolor.max(Vec3::ZERO).powf(0.5), self.thin_ggx())
            } else {
                self.refraction(self.basecolor, ggx)
            };
            ret += self.spec_trans * (1.0 - self.metallic) * mr.f(wo, wi);
        }

        // Diffuse transmission
        if self.thin_surface && self.diff_trans > 0.0 && diffuse_weight > 0.0 {
            ret += self.diff_trans * diffuse_weight * lambert_transmission_f(self.basecolor, wo, wi);
        }

        ret
    }

    pub fn sample(&self, wo: Vec3, bs: (f32, f32), rng: &mut Rng) -> Option<(Spectrum, Vec3, f32)> {
        let lobes = self.lobes();
        if lobes.total <= 0.0 {
            return None;
        }
        let inv_total = 1.0 / lobes.total;
        let cc_w = lobes.clearcoat * inv_total;
        let sr_w = lobes.specular_reflection * inv_total + cc_w;
        let st_w = lobes.specular_transmission * inv_total + sr_w;
        let dr_w = lobes.diffuse_reflection * inv_total + st_w;

        let r = rng.f32();
        let wi = if r < cc_w {
            let wh = self.clearcoat_ggx().sample(rng.pair());
            reflect(wo, wh)
        } else if r < sr_w {
            let wh = self.ggx().sample(rng.pair());
            reflect(wo, wh)
        } else if r < st_w {
            let dist = if self.thin_surface { self.thin_ggx() } else { self.ggx() };
            self.refraction(Spectrum::ONE, dist).sample_dir(wo, bs)?
        } else if r < dr_w || !self.thin_surface {
            cos_sample_hemisphere(rng.f32(), rng.f32())
        } else {
            lambert_transmission_sample(wo, bs)
        };

        let pdf = self.pdf(wo, wi);
        if !(pdf > 0.0) {
            return None;
        }
        Some((self.f(wo, wi), wi, pdf))
    }

    pub fn pdf(&self, wo: Vec3, wi: Vec3) -> f32 {
        let lobes = self.lobes();
        if lobes.total <= 0.0 {
            return 0.0;
        }

        let mut total_pdf = 0.0;
        let wh = (wi + wo).normalize_or_zero();
        let o_o_h = wo.dot(wh).abs();
        let ggx = self.ggx();
        if lobes.clearcoat > 0.0 && o_o_h > 0.0 {
            total_pdf += lobes.clearcoat * self.clearcoat_ggx().pdf(wh) / (4.0 * o_o_h);
        }
        if lobes.specular_reflection > 0.0 && o_o_h > 0.0 {
            total_pdf += lobes.specular_reflection * ggx.pdf(wh) / (4.0 * o_o_h);
        }
        if lobes.specular_transmission > 0.0 {
            let dist = if self.thin_surface { self.thin_ggx() } else { ggx };
            total_pdf += lobes.specular_transmission * self.refraction(Spectrum::ONE, dist).pdf(wo, wi);
        }
        // the diffuse lobe only ever samples the upper hemisphere
        if lobes.diffuse_reflection > 0.0 && cos_theta(wi) > 0.0 {
            total_pdf += lobes.diffuse_reflection * cos_hemisphere_pdf(wi);
        }
        if lobes.diffuse_transmission > 0.0 {
            total_pdf += lobes.diffuse_transmission * lambert_transmission_pdf(wo, wi);
        }
        total_pdf / lobes.total
    }
}

// ---------------------------------------------------------------------------------------------
// Lambert ( lambert.cpp )
// ---------------------------------------------------------------------------------------------

#[inline]
fn lambert_f(r: Spectrum, wo: Vec3, wi: Vec3) -> Spectrum {
    if !same_hemisphere(wo, wi) || cos_theta(wo) <= 0.0 {
        return Spectrum::ZERO;
    }
    r * (INV_PI * abs_cos_theta(wi))
}

#[inline]
fn lambert_pdf(wo: Vec3, wi: Vec3) -> f32 {
    if !same_hemisphere(wo, wi) || cos_theta(wo) <= 0.0 {
        return 0.0;
    }
    cos_hemisphere_pdf(wi)
}

#[inline]
fn lambert_transmission_f(t: Spectrum, wo: Vec3, wi: Vec3) -> Spectrum {
    if same_hemisphere(wo, wi) {
        Spectrum::ZERO
    } else {
        t * (INV_PI * abs_cos_theta(wi))
    }
}

#[inline]
fn lambert_transmission_sample(wo: Vec3, bs: (f32, f32)) -> Vec3 {
    let wi = -cos_sample_hemisphere(bs.0, bs.1);
    if same_hemisphere(wi, wo) {
        -wi
    } else {
        wi
    }
}

#[inline]
fn lambert_transmission_pdf(wo: Vec3, wi: Vec3) -> f32 {
    if same_hemisphere(wo, wi) {
        0.0
    } else {
        cos_hemisphere_pdf(wi)
    }
}

// ---------------------------------------------------------------------------------------------
// BXDF and scattering event ( scatteringevent.cpp )
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub enum Bxdf {
    Lambert(Spectrum),
    LambertTransmission(Spectrum),
    MicrofacetReflection(MicrofacetReflection),
    MicrofacetRefraction(MicrofacetRefraction),
    Disney(Disney),
    /// Straight-through transmission with an attenuation, a delta distribution.
    Transparent(Spectrum),
}

impl Bxdf {
    #[inline]
    pub fn is_delta(&self) -> bool {
        matches!(self, Bxdf::Transparent(_))
    }

    pub fn f(&self, wo: Vec3, wi: Vec3) -> Spectrum {
        match self {
            Bxdf::Lambert(r) => lambert_f(*r, wo, wi),
            Bxdf::LambertTransmission(t) => lambert_transmission_f(*t, wo, wi),
            Bxdf::MicrofacetReflection(m) => m.f(wo, wi),
            Bxdf::MicrofacetRefraction(m) => m.f(wo, wi),
            Bxdf::Disney(d) => d.f(wo, wi),
            Bxdf::Transparent(_) => Spectrum::ZERO,
        }
    }

    pub fn pdf(&self, wo: Vec3, wi: Vec3) -> f32 {
        match self {
            Bxdf::Lambert(_) => lambert_pdf(wo, wi),
            Bxdf::LambertTransmission(_) => lambert_transmission_pdf(wo, wi),
            Bxdf::MicrofacetReflection(m) => m.pdf(wo, wi),
            Bxdf::MicrofacetRefraction(m) => m.pdf(wo, wi),
            Bxdf::Disney(d) => d.pdf(wo, wi),
            Bxdf::Transparent(_) => 0.0,
        }
    }

    /// Sample an incident direction. Returns the BXDF value, the direction and its pdf.
    pub fn sample(&self, wo: Vec3, bs: (f32, f32), rng: &mut Rng) -> Option<(Spectrum, Vec3, f32)> {
        match self {
            Bxdf::Lambert(r) => {
                let wi = cos_sample_hemisphere(bs.0, bs.1);
                let pdf = lambert_pdf(wo, wi);
                (pdf > 0.0).then(|| (lambert_f(*r, wo, wi), wi, pdf))
            }
            Bxdf::LambertTransmission(t) => {
                let wi = lambert_transmission_sample(wo, bs);
                let pdf = lambert_transmission_pdf(wo, wi);
                (pdf > 0.0).then(|| (lambert_transmission_f(*t, wo, wi), wi, pdf))
            }
            Bxdf::MicrofacetReflection(m) => m.sample(wo, bs),
            Bxdf::MicrofacetRefraction(m) => m.sample(wo, bs),
            Bxdf::Disney(d) => d.sample(wo, bs, rng),
            Bxdf::Transparent(a) => Some((*a, -wo, 1.0)),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ScatteringUnit {
    bxdf: Bxdf,
    eval_weight: Spectrum,
    sample_weight: f32,
}

pub const MAX_BXDF_COUNT: usize = 4;

/// Result of sampling the BSDF of a scattering event.
#[derive(Clone, Copy, Debug)]
pub struct BsdfSampleResult {
    pub f: Spectrum,
    /// Sampled incident direction in world space.
    pub wi: Vec3,
    pub pdf: f32,
    /// The direction comes from a delta distribution, `pdf` is a discrete probability.
    pub delta: bool,
}

/// The collection of BXDFs describing how light scatters at a surface point.
#[derive(Clone, Debug)]
pub struct ScatteringEvent {
    n: Vec3,
    t: Vec3,
    bt: Vec3,
    units: [ScatteringUnit; MAX_BXDF_COUNT],
    count: usize,
    total_sample_weight: f32,
    /// Some BXDF lets light through the surface.
    transmissive: bool,
}

impl ScatteringEvent {
    pub fn new(normal: Vec3, tangent: Vec3) -> Self {
        let n = normal.normalize();
        let mut bt = n.cross(tangent);
        if bt.length_squared() < 1e-12 {
            bt = coordinate_system(n).1;
        }
        let bt = bt.normalize();
        let t = bt.cross(n).normalize();
        let empty = ScatteringUnit {
            bxdf: Bxdf::Lambert(Spectrum::ZERO),
            eval_weight: Spectrum::ZERO,
            sample_weight: 0.0,
        };
        Self { n, t, bt, units: [empty; MAX_BXDF_COUNT], count: 0, total_sample_weight: 0.0, transmissive: false }
    }

    /// Add a BXDF with separate evaluation and sampling weights.
    pub fn add_bxdf(&mut self, bxdf: Bxdf, eval_weight: Spectrum, sample_weight: f32) {
        if self.count == MAX_BXDF_COUNT || is_black(eval_weight) || sample_weight <= 0.0 {
            return;
        }
        self.transmissive |= match &bxdf {
            Bxdf::Lambert(_) | Bxdf::MicrofacetReflection(_) => false,
            Bxdf::LambertTransmission(_) | Bxdf::MicrofacetRefraction(_) | Bxdf::Transparent(_) => true,
            Bxdf::Disney(d) => d.spec_trans > 0.0 || (d.thin_surface && d.diff_trans > 0.0),
        };
        self.units[self.count] = ScatteringUnit { bxdf, eval_weight, sample_weight };
        self.count += 1;
        self.total_sample_weight += sample_weight;
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    #[inline]
    pub fn normal(&self) -> Vec3 {
        self.n
    }

    /// Cheap test that rules out incident directions the BSDF is certainly zero for: the ones
    /// below the surface when nothing transmits light.
    #[inline]
    pub fn may_scatter(&self, wi: Vec3) -> bool {
        self.transmissive || wi.dot(self.n) > 0.0
    }

    #[inline]
    pub fn world_to_local(&self, v: Vec3) -> Vec3 {
        Vec3::new(v.dot(self.t), v.dot(self.n), v.dot(self.bt))
    }

    #[inline]
    pub fn local_to_world(&self, v: Vec3) -> Vec3 {
        self.t * v.x + self.n * v.y + self.bt * v.z
    }

    #[inline]
    fn units(&self) -> &[ScatteringUnit] {
        &self.units[..self.count]
    }

    pub fn evaluate_bsdf(&self, wo: Vec3, wi: Vec3) -> Spectrum {
        let (swo, swi) = (self.world_to_local(wo), self.world_to_local(wi));
        self.units().iter().map(|u| u.bxdf.f(swo, swi) * u.eval_weight).sum()
    }

    pub fn pdf_bsdf(&self, wo: Vec3, wi: Vec3) -> f32 {
        if self.count == 0 {
            return 0.0;
        }
        let (swo, swi) = (self.world_to_local(wo), self.world_to_local(wi));
        self.units().iter().map(|u| u.bxdf.pdf(swo, swi) * u.sample_weight).sum::<f32>()
            / self.total_sample_weight
    }

    /// Pick one BXDF proportionally to its sampling weight, sample a direction from it and
    /// evaluate the whole BSDF for that direction.
    ///
    /// SORT evaluates the BXDFs that were not picked with a world space `wo` and without
    /// normalizing their sampling weights; here everything is evaluated in the local frame and
    /// the pdf is the properly weighted mixture.
    pub fn sample_bsdf(&self, wo: Vec3, bs: (f32, f32), rng: &mut Rng) -> Option<BsdfSampleResult> {
        if self.count == 0 {
            return None;
        }

        let mut picked = self.count - 1;
        let mut r = rng.f32() * self.total_sample_weight;
        for (i, u) in self.units().iter().enumerate() {
            if r <= u.sample_weight {
                picked = i;
                break;
            }
            r -= u.sample_weight;
        }
        let unit = &self.units[picked];
        let pick_pdf = unit.sample_weight / self.total_sample_weight;

        let swo = self.world_to_local(wo);
        let (f, swi, pdf) = unit.bxdf.sample(swo, bs, rng)?;
        if pdf == 0.0 {
            return None;
        }

        if unit.bxdf.is_delta() {
            return Some(BsdfSampleResult {
                f: f * unit.eval_weight,
                wi: self.local_to_world(swi),
                pdf: pdf * pick_pdf,
                delta: true,
            });
        }

        let mut f = f * unit.eval_weight;
        let mut pdf = pdf * pick_pdf;
        for (i, u) in self.units().iter().enumerate() {
            if i != picked {
                f += u.bxdf.f(swo, swi) * u.eval_weight;
                pdf += u.bxdf.pdf(swo, swi) * u.sample_weight / self.total_sample_weight;
            }
        }
        Some(BsdfSampleResult { f, wi: self.local_to_world(swi), pdf, delta: false })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Checks that importance sampling and `f`/`pdf` agree: the estimate of the albedo using
    /// the BXDF's own sampling must match the one using uniform sphere sampling, and the pdf
    /// must integrate to (at most) one.
    fn check_bxdf(name: &str, bxdf: Bxdf, wo: Vec3) {
        let mut rng = Rng::new(0x5eed, 7);
        let n = 400_000;

        let mut importance = Spectrum::ZERO;
        for _ in 0..n {
            let bs = rng.pair();
            if let Some((f, wi, pdf)) = bxdf.sample(wo, bs, &mut rng) {
                assert!(pdf > 0.0 && f.is_finite(), "{name}: bad sample f={f:?} pdf={pdf}");
                let expected = bxdf.pdf(wo, wi);
                assert!(
                    (expected - pdf).abs() <= 1e-3 * pdf.max(1.0),
                    "{name}: pdf mismatch {expected} vs {pdf}"
                );
                importance += f / pdf;
            }
        }
        importance /= n as f32;

        let mut uniform = Spectrum::ZERO;
        let mut pdf_integral = 0.0f64;
        for _ in 0..n {
            let (u, v) = rng.pair();
            let wi = uniform_sample_sphere(u, v);
            uniform += bxdf.f(wo, wi) / uniform_sphere_pdf();
            pdf_integral += (bxdf.pdf(wo, wi) / uniform_sphere_pdf()) as f64;
        }
        uniform /= n as f32;
        let pdf_integral = pdf_integral / n as f64;

        let tolerance = 0.03 * uniform.max_element().max(0.05);
        assert!(
            (importance - uniform).abs().max_element() < tolerance,
            "{name}: importance {importance:?} vs uniform {uniform:?}"
        );
        assert!(pdf_integral < 1.03, "{name}: pdf integrates to {pdf_integral}");
        assert!(importance.max_element() < 1.05, "{name}: creates energy {importance:?}");
    }

    fn dir(theta_deg: f32, phi_deg: f32) -> Vec3 {
        spherical_vec(theta_deg.to_radians(), phi_deg.to_radians())
    }

    #[test]
    fn lambert_is_consistent() {
        check_bxdf("lambert", Bxdf::Lambert(Spectrum::splat(0.7)), dir(40.0, 20.0));
        check_bxdf("lambert transmission", Bxdf::LambertTransmission(Spectrum::splat(0.7)), dir(40.0, 20.0));
    }

    #[test]
    fn microfacet_reflection_is_consistent() {
        for (ru, rv) in [(0.5, 0.5), (0.8, 0.3), (0.3, 0.6)] {
            let bxdf = Bxdf::MicrofacetReflection(MicrofacetReflection {
                r: Spectrum::ONE,
                dist: Ggx::new(ru, rv),
                fresnel: Fresnel::Dielectric { eta_i: 1.0, eta_t: 1.5 },
            });
            check_bxdf("microfacet reflection", bxdf, dir(50.0, 35.0));
        }
    }

    #[test]
    fn microfacet_refraction_is_consistent() {
        let bxdf = Bxdf::MicrofacetRefraction(MicrofacetRefraction {
            t: Spectrum::ONE,
            dist: Ggx::new(0.6, 0.6),
            eta_i: 1.0,
            eta_t: 1.5,
        });
        check_bxdf("microfacet refraction", bxdf, dir(35.0, 10.0));
        // leaving the surface from inside
        check_bxdf("microfacet refraction (inside)", bxdf, -dir(25.0, 10.0));
    }

    #[test]
    fn disney_is_consistent() {
        let base = Disney { basecolor: Spectrum::new(0.8, 0.6, 0.4), roughness: 0.6, ..Default::default() };
        let cases = [
            ("plastic", base),
            ("metal", Disney { metallic: 1.0, roughness: 0.5, ..base }),
            ("anisotropic metal", Disney { metallic: 1.0, anisotropic: 0.8, ..base }),
            ("clearcoat", Disney { clearcoat: 1.0, clearcoat_gloss: 0.3, sheen: 0.5, ..base }),
            ("glass", Disney { spec_trans: 1.0, roughness: 0.7, ..base }),
            ("thin", Disney { thin_surface: true, diff_trans: 0.5, flatness: 0.3, ..base }),
        ];
        for (name, d) in cases {
            check_bxdf(name, Bxdf::Disney(d), dir(45.0, 30.0));
        }
    }

    #[test]
    fn scattering_event_mixture_is_consistent() {
        let mut rng = Rng::new(11, 3);
        let mut se = ScatteringEvent::new(Vec3::new(0.2, 0.9, 0.1), Vec3::X);
        se.add_bxdf(Bxdf::Lambert(Spectrum::splat(0.5)), Spectrum::splat(0.6), 0.6);
        se.add_bxdf(
            Bxdf::MicrofacetReflection(MicrofacetReflection {
                r: Spectrum::ONE,
                dist: Ggx::new(0.5, 0.5),
                fresnel: Fresnel::Schlick(Spectrum::splat(0.04)),
            }),
            Spectrum::ONE,
            0.4,
        );
        let wo = se.local_to_world(dir(40.0, 60.0));
        let n = 300_000;
        let (mut importance, mut uniform) = (Spectrum::ZERO, Spectrum::ZERO);
        for _ in 0..n {
            let bs = rng.pair();
            if let Some(s) = se.sample_bsdf(wo, bs, &mut rng) {
                assert!((se.pdf_bsdf(wo, s.wi) - s.pdf).abs() <= 1e-3 * s.pdf.max(1.0));
                importance += s.f / s.pdf;
            }
            let (u, v) = rng.pair();
            let wi = uniform_sample_sphere(u, v);
            uniform += se.evaluate_bsdf(wo, wi) / uniform_sphere_pdf();
        }
        let (importance, uniform) = (importance / n as f32, uniform / n as f32);
        assert!((importance - uniform).abs().max_element() < 0.02, "{importance:?} vs {uniform:?}");
    }

    #[test]
    fn fresnel_limits() {
        assert!((dielectric_fresnel(1.0, 1.0, 1.5) - 0.04).abs() < 1e-4);
        assert!((dielectric_fresnel(0.0, 1.0, 1.5) - 1.0).abs() < 1e-4);
        // total internal reflection
        assert_eq!(dielectric_fresnel(-0.2, 1.0, 1.5), 1.0);
    }
}
