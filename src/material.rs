// This file is a part of SORT-rs, a Rust port of SORT (Simple Open Ray Tracing).
// Original work Copyright (c) 2011-2023 by Jiayin Cao, GPLv3.

//! Materials and textures.
//!
//! SORT describes materials with Tiny Shading Language shaders that output a tree of closures.
//! This port has no shading language: a material is a fixed set of parameters that is turned
//! into the same closures (Disney, Transparent, microfacet reflection) at every hit.

use crate::bsdf::{Bxdf, Disney, Fresnel, Ggx, MicrofacetReflection, ScatteringEvent};
use crate::math::*;
use std::sync::{Arc, OnceLock};

/// An 8-bit RGBA texture, sampled with bilinear filtering and repeat wrapping.
pub struct Texture {
    pub width: usize,
    pub height: usize,
    pub data: Vec<[u8; 4]>,
    /// Color channels are sRGB encoded and decoded to linear when sampled.
    pub srgb: bool,
}

fn srgb_lut() -> &'static [f32; 256] {
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = [0.0; 256];
        for (i, v) in lut.iter_mut().enumerate() {
            let c = i as f32 / 255.0;
            *v = if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        }
        lut
    })
}

impl Texture {
    #[inline]
    fn texel(&self, x: usize, y: usize) -> Vec4 {
        let p = self.data[y * self.width + x];
        if self.srgb {
            let lut = srgb_lut();
            Vec4::new(lut[p[0] as usize], lut[p[1] as usize], lut[p[2] as usize], p[3] as f32 / 255.0)
        } else {
            Vec4::new(p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32) / 255.0
        }
    }

    pub fn sample(&self, uv: Vec2) -> Vec4 {
        if self.width == 0 || self.height == 0 {
            return Vec4::ONE;
        }
        let x = (uv.x - uv.x.floor()) * self.width as f32 - 0.5;
        let y = (uv.y - uv.y.floor()) * self.height as f32 - 0.5;
        let (fx, fy) = (x.floor(), y.floor());
        let (tx, ty) = (x - fx, y - fy);
        let wrap = |v: f32, n: usize| (v as i64).rem_euclid(n as i64) as usize;
        let (x0, x1) = (wrap(fx, self.width), wrap(fx + 1.0, self.width));
        let (y0, y1) = (wrap(fy, self.height), wrap(fy + 1.0, self.height));
        let top = self.texel(x0, y0).lerp(self.texel(x1, y0), tx);
        let bottom = self.texel(x0, y1).lerp(self.texel(x1, y1), tx);
        top.lerp(bottom, ty)
    }
}

/// Reflectance of glass at normal incidence, used to split energy for thin glass.
const GLASS_F0: f32 = 0.04;

#[derive(Clone)]
pub struct Material {
    pub name: String,
    pub base_color: Spectrum,
    pub base_color_tex: Option<Arc<Texture>>,
    pub metallic: f32,
    pub roughness: f32,
    /// Roughness in the green channel, metallic in the blue channel (glTF convention).
    pub metallic_roughness_tex: Option<Arc<Texture>>,
    pub normal_tex: Option<Arc<Texture>>,
    pub normal_scale: f32,
    /// Emitted radiance.
    pub emissive: Spectrum,
    pub emissive_tex: Option<Arc<Texture>>,
    /// Amount of specular transmission.
    pub transmission: f32,
    /// The surface is an infinitely thin sheet (a window pane modelled as a single face).
    /// Light goes straight through it and shadow rays are attenuated instead of blocked.
    pub thin: bool,
    pub ior: f32,
    pub specular: f32,
    pub specular_tint: f32,
    pub anisotropic: f32,
    pub sheen: f32,
    pub sheen_tint: f32,
    pub clearcoat: f32,
    pub clearcoat_gloss: f32,
    /// Shade and emit on both sides of the surface.
    pub double_sided: bool,
    /// Geometry with this material is not rendered. It marks an opening (a window, a door, a
    /// skylight) that sky light comes through, and is used to aim sky samples at it.
    pub portal: bool,
}

impl Default for Material {
    fn default() -> Self {
        Self {
            name: String::new(),
            base_color: Spectrum::splat(0.8),
            base_color_tex: None,
            metallic: 0.0,
            roughness: 0.5,
            metallic_roughness_tex: None,
            normal_tex: None,
            normal_scale: 1.0,
            emissive: Spectrum::ZERO,
            emissive_tex: None,
            transmission: 0.0,
            thin: false,
            ior: 1.5,
            specular: 1.0,
            specular_tint: 0.0,
            anisotropic: 0.0,
            sheen: 0.0,
            sheen_tint: 0.0,
            clearcoat: 0.0,
            clearcoat_gloss: 1.0,
            double_sided: true,
            portal: false,
        }
    }
}

impl Material {
    #[inline]
    pub fn is_emissive(&self) -> bool {
        !is_black(self.emissive)
    }

    /// Solid transmissive objects need to know which side of the surface a ray is on, so their
    /// normal is never flipped toward the viewer.
    #[inline]
    pub fn has_interior(&self) -> bool {
        self.transmission > 0.0 && !self.thin
    }

    #[inline]
    pub fn base_color_at(&self, uv: Vec2) -> Spectrum {
        match &self.base_color_tex {
            Some(tex) => self.base_color * tex.sample(uv).truncate(),
            None => self.base_color,
        }
    }

    /// Emitted radiance at a surface point.
    #[inline]
    pub fn emission(&self, uv: Vec2) -> Spectrum {
        match &self.emissive_tex {
            Some(tex) => self.emissive * tex.sample(uv).truncate(),
            None => self.emissive,
        }
    }

    /// Fraction of light a shadow ray keeps when crossing the surface, `None` if it is opaque.
    #[inline]
    pub fn shadow_transmittance(&self, uv: Vec2) -> Option<Spectrum> {
        if !self.thin || self.transmission <= 0.0 {
            return None;
        }
        Some(self.base_color_at(uv) * (self.transmission * (1.0 - GLASS_F0) * (1.0 - self.metallic)))
    }

    /// Tangent space normal from the normal map, if any.
    #[inline]
    pub fn normal_at(&self, uv: Vec2) -> Option<Vec3> {
        let tex = self.normal_tex.as_ref()?;
        let n = tex.sample(uv).truncate() * 2.0 - Vec3::ONE;
        Some(Vec3::new(n.x * self.normal_scale, n.y * self.normal_scale, n.z))
    }

    /// Populate a scattering event, the equivalent of `MaterialBase::UpdateScatteringEvent`.
    ///
    /// `min_alpha` is a lower bound for the width of the specular lobes (the GGX alpha, the
    /// square of the roughness), zero for the material as authored. See
    /// `PathTracing::filter_glossy`.
    pub fn update_scattering_event(&self, se: &mut ScatteringEvent, uv: Vec2, min_alpha: f32) {
        let basecolor = self.base_color_at(uv);
        let (mut roughness, mut metallic) = (self.roughness, self.metallic);
        if let Some(tex) = &self.metallic_roughness_tex {
            let mr = tex.sample(uv);
            roughness *= mr.y;
            metallic *= mr.z;
        }
        let mut clearcoat_gloss = self.clearcoat_gloss;
        if min_alpha > 0.0 {
            roughness = roughness.max(min_alpha.sqrt());
            // the alpha of the coat goes from 0.1 down to 0.001 as the gloss goes from 0 to 1
            clearcoat_gloss = clearcoat_gloss.min(((0.1 - min_alpha) / 0.099).clamp(0.0, 1.0));
        }

        let disney = Disney {
            basecolor,
            metallic,
            specular: self.specular,
            specular_tint: self.specular_tint,
            roughness,
            anisotropic: self.anisotropic,
            sheen: self.sheen,
            sheen_tint: self.sheen_tint,
            clearcoat: self.clearcoat,
            clearcoat_gloss,
            spec_trans: if self.thin { 0.0 } else { self.transmission },
            ior: self.ior,
            ..Default::default()
        };

        let glass = if self.thin { self.transmission * (1.0 - metallic) } else { 0.0 };
        if glass <= 0.0 {
            se.add_bxdf(Bxdf::Disney(disney), Spectrum::ONE, 1.0);
            return;
        }

        // Thin glass: a blend between the opaque surface and a pane made of a straight-through
        // transmission plus a dielectric reflection, the way a window is authored in SORT with
        // a Transparent closure.
        se.add_bxdf(Bxdf::Disney(disney), Spectrum::splat(1.0 - glass), 1.0 - glass);
        se.add_bxdf(Bxdf::Transparent(basecolor), Spectrum::splat(glass * (1.0 - GLASS_F0)), glass * 0.85);
        se.add_bxdf(
            Bxdf::MicrofacetReflection(MicrofacetReflection {
                r: Spectrum::ONE,
                dist: Ggx::new(roughness, roughness),
                fresnel: Fresnel::Dielectric { eta_i: 1.0, eta_t: self.ior },
            }),
            Spectrum::splat(glass),
            glass * 0.15,
        );
    }
}
