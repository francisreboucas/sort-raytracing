// This file is a part of SORT-rs, a Rust port of SORT (Simple Open Ray Tracing).
// Original work Copyright (c) 2011-2023 by Jiayin Cao, GPLv3.

//! Materials and textures.
//!
//! SORT describes materials with Tiny Shading Language shaders that output a tree of closures.
//! This port has no shading language: a material is a fixed set of parameters that is turned
//! into the same closures (Disney, coat, cloth, Transparent, microfacet reflection) at every
//! hit.

use crate::bsdf::{
    Bxdf, Coat, Disney, DistributionBrdf, Fabric, Fresnel, Ggx, MicrofacetReflection, ScatteringEvent,
};
use crate::math::*;
use crate::medium::Medium;
use glam::{Affine2, Mat2};
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
    pub fn texel(&self, x: usize, y: usize) -> Vec4 {
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

/// A texture with the transform applied to the texture coordinates before sampling it.
#[derive(Clone)]
pub struct TextureMap {
    pub texture: Arc<Texture>,
    /// Maps the mesh texture coordinates to the texture's, `None` for the identity.
    pub transform: Option<Affine2>,
}

impl From<Arc<Texture>> for TextureMap {
    fn from(texture: Arc<Texture>) -> Self {
        Self { texture, transform: None }
    }
}

impl TextureMap {
    #[inline]
    pub fn sample(&self, uv: Vec2) -> Vec4 {
        match &self.transform {
            Some(t) => self.texture.sample(t.transform_point2(uv)),
            None => self.texture.sample(uv),
        }
    }

    /// The transform of glTF's `KHR_texture_transform`: uv' = T(offset) * R(rotation) *
    /// S(scale) * uv. The rotation turns the UVs counter-clockwise as seen with v pointing
    /// down (the image turns clockwise), which is `[cos sin; -sin cos]` written row by row,
    /// the way the extension's example (offset [0, 1], rotation 90 degrees, scale 0.5 picks
    /// the lower left quadrant) and three.js read it.
    pub fn khr_texture_transform(offset: Vec2, rotation: f32, scale: Vec2) -> Affine2 {
        let (s, c) = rotation.sin_cos();
        let rotation = Mat2::from_cols(Vec2::new(c, -s), Vec2::new(s, c));
        Affine2::from_mat2_translation(rotation * Mat2::from_diagonal(scale), offset)
    }
}

/// How the alpha of the base color is used (glTF `alphaMode`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AlphaMode {
    /// Alpha is ignored.
    Opaque,
    /// The surface exists where alpha >= `cutoff` and is a hole elsewhere: leaves, fences,
    /// perforated screens.
    Mask { cutoff: f32 },
    /// The surface covers a fraction alpha of each point; light goes straight through the
    /// rest unchanged: sheer curtains, decals that fade out.
    Blend,
}

/// Reflectance of glass at normal incidence, used to split energy for thin glass.
const GLASS_F0: f32 = 0.04;

/// Largest albedo of the sheen lobe for a white sheen color, over all roughnesses and
/// directions (0.83, at the horizon, measured by quadrature). The base under a sheen is scaled
/// by `1 - max(sheen_color) * SHEEN_ALBEDO`, so the pair never reflects more than it receives.
const SHEEN_ALBEDO: f32 = 0.85;

/// A pure cloth material: one of SORT's cloth BRDFs replaces the Disney BRDF, as SORT's Cloth
/// material node does. It is lit with `Material::base_color` (and its texture).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cloth {
    /// The DreamWorks fabric (`fabric.cpp`): woven threads along the tangent of the surface
    /// (the U direction of the texture coordinates) that reflect light around the plane
    /// across them. Low roughness gives a narrow, shiny band; 1 a broad, dull one.
    Fabric { roughness: f32 },
    /// The distribution-based cloth of The Order: 1886 (`distributionbrdf.cpp`): a diffuse
    /// base plus a lobe that brightens towards the silhouette, as velvet and upholstery do.
    /// `specular` is the reflectance of that lobe at normal incidence (0 to 1), and
    /// `specular_tint` tints it towards the base color.
    Distribution { roughness: f32, specular: f32, specular_tint: f32 },
}

#[derive(Clone)]
pub struct Material {
    pub name: String,
    pub base_color: Spectrum,
    /// Base color in RGB, alpha (coverage) in A.
    pub base_color_tex: Option<TextureMap>,
    /// Alpha of the base color factor, multiplied by the texture's.
    pub alpha: f32,
    pub alpha_mode: AlphaMode,
    pub metallic: f32,
    pub roughness: f32,
    /// Roughness in the green channel, metallic in the blue channel (glTF convention).
    pub metallic_roughness_tex: Option<TextureMap>,
    pub normal_tex: Option<TextureMap>,
    pub normal_scale: f32,
    /// Emitted radiance.
    pub emissive: Spectrum,
    pub emissive_tex: Option<TextureMap>,
    /// Amount of specular transmission.
    pub transmission: f32,
    /// The surface is an infinitely thin sheet (a window pane modelled as a single face).
    /// Light goes straight through it and shadow rays are attenuated instead of blocked.
    pub thin: bool,
    pub ior: f32,
    /// What fills the inside of a solid transmissive object (not `thin`): thick tinted glass,
    /// the water of a pool. Light that crosses it is absorbed and scattered along the way, so
    /// thick parts are darker than thin ones (glTF `KHR_materials_volume`). The faces must
    /// point outwards. A camera placed inside the object does not see the medium.
    pub medium: Option<Medium>,
    pub specular: f32,
    pub specular_tint: f32,
    pub anisotropic: f32,
    /// Sheen of the Disney BRDF: a little white or tinted light added at grazing angles.
    pub sheen: f32,
    pub sheen_tint: f32,
    /// Clearcoat of the Disney BRDF: an extra specular lobe added on top, the base is not
    /// changed. See `coat` for a layer the base is seen through.
    pub clearcoat: f32,
    pub clearcoat_gloss: f32,
    /// A coat of varnish or lacquer over the base (SORT's coat closure, glTF
    /// `KHR_materials_clearcoat`): 0 for none, 1 for a full coat, in between a blend of the
    /// coated and the bare surface. Unlike `clearcoat`, light is refracted into the layer:
    /// the base is seen through it, attenuated by its Fresnel transmission and tinted by
    /// `coat_color`, and the coat reflects the light the base does not get. It applies to the
    /// Disney BRDF, not to `cloth`, and is ignored on solid transmissive materials (SORT's
    /// coat does not support glass either).
    pub coat: f32,
    /// Multiplies `coat` by its red channel (glTF `clearcoatTexture`).
    pub coat_tex: Option<TextureMap>,
    pub coat_roughness: f32,
    /// Multiplies `coat_roughness` by its green channel (glTF `clearcoatRoughnessTexture`).
    pub coat_roughness_tex: Option<TextureMap>,
    pub coat_ior: f32,
    /// What the coat lets through of the light that crosses it straight down and back up:
    /// white is clear. It sets the absorption of the layer, as SORT's coat has; the base of
    /// a light material is tinted more deeply than this, light bouncing several times
    /// inside the layer.
    pub coat_color: Spectrum,
    /// Color of a sheen layered over the base, as on velvet, upholstery and curtains (glTF
    /// `KHR_materials_sheen`); black for none. It is SORT's distribution-based cloth lobe
    /// (`Cloth::Distribution`); the base under it is darkened by `1 - 0.85 max(sheen_color)`,
    /// a conservative bound of what the sheen reflects, so the two never reflect more light
    /// than they receive.
    pub sheen_color: Spectrum,
    /// Multiplies `sheen_color` by its RGB (glTF `sheenColorTexture`).
    pub sheen_color_tex: Option<TextureMap>,
    /// Width of the sheen, 0 to 1: low keeps it close to the silhouette.
    pub sheen_roughness: f32,
    /// Multiplies `sheen_roughness` by its alpha channel (glTF `sheenRoughnessTexture`).
    pub sheen_roughness_tex: Option<TextureMap>,
    /// A pure cloth material instead of the Disney BRDF, see `Cloth`. Ignored on solid
    /// transmissive materials.
    pub cloth: Option<Cloth>,
    /// Material of the back of the surface, as an index into the scene's materials (SORT's
    /// double-sided closure): a sheet with a different finish on each face. It is used when
    /// a ray arrives on the back face (against the geometric normal) of a surface that has
    /// no interior. Everything else comes from this material whatever the side: emission
    /// (which follows `double_sided`), alpha cutouts and coverage, and what shadow rays
    /// see, so that tracing rays does not depend on the side they come from. An index out of
    /// range is dropped by `SceneBuilder::build`.
    pub back_material: Option<u32>,
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
            alpha: 1.0,
            alpha_mode: AlphaMode::Opaque,
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
            medium: None,
            specular: 1.0,
            specular_tint: 0.0,
            anisotropic: 0.0,
            sheen: 0.0,
            sheen_tint: 0.0,
            clearcoat: 0.0,
            clearcoat_gloss: 1.0,
            coat: 0.0,
            coat_tex: None,
            coat_roughness: 0.0,
            coat_roughness_tex: None,
            coat_ior: 1.5,
            coat_color: Spectrum::ONE,
            sheen_color: Spectrum::ZERO,
            sheen_color_tex: None,
            sheen_roughness: 0.0,
            sheen_roughness_tex: None,
            cloth: None,
            back_material: None,
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
        self.base_color_and_alpha(uv).0
    }

    /// Base color and alpha, from a single lookup of the texture.
    #[inline]
    fn base_color_and_alpha(&self, uv: Vec2) -> (Spectrum, f32) {
        match &self.base_color_tex {
            Some(tex) => {
                let texel = tex.sample(uv);
                (self.base_color * texel.truncate(), self.alpha * texel.w)
            }
            None => (self.base_color, self.alpha),
        }
    }

    #[inline]
    fn opacity_from_alpha(&self, alpha: f32) -> f32 {
        match self.alpha_mode {
            AlphaMode::Opaque => 1.0,
            AlphaMode::Mask { cutoff } => {
                if alpha >= cutoff {
                    1.0
                } else {
                    0.0
                }
            }
            AlphaMode::Blend => alpha.clamp(0.0, 1.0),
        }
    }

    /// Fraction of a surface point that is there: 1 for opaque materials, 0 or 1 for masked
    /// ones, the alpha for blended ones.
    #[inline]
    pub fn opacity_at(&self, uv: Vec2) -> f32 {
        match self.alpha_mode {
            AlphaMode::Opaque => 1.0,
            _ => self.opacity_from_alpha(self.base_color_and_alpha(uv).1),
        }
    }

    /// Opacity of a point a ray did hit: the holes of a masked material are never hit (see
    /// `Scene`), so only blending leaves part of the light through.
    #[inline]
    pub fn coverage(&self, uv: Vec2) -> f32 {
        match self.alpha_mode {
            AlphaMode::Blend => self.opacity_at(uv),
            _ => 1.0,
        }
    }

    /// Whether some points of the surface are holes, which ray traversal has to skip.
    pub fn has_cutouts(&self) -> bool {
        match self.alpha_mode {
            AlphaMode::Mask { cutoff } => self.base_color_tex.is_some() || self.alpha < cutoff,
            _ => false,
        }
    }

    /// Whether `shadow_transmittance` can be anything but `None`.
    pub fn lets_shadows_through(&self) -> bool {
        let blend = self.alpha_mode == AlphaMode::Blend && (self.base_color_tex.is_some() || self.alpha < 1.0);
        blend || (self.thin && self.transmission > 0.0)
    }

    /// Emitted radiance at a surface point. A partially covering surface emits in proportion
    /// to its coverage, and the holes of a masked one not at all, which light sampling (that
    /// may pick points in the holes) and paths that hit the surface agree on.
    #[inline]
    pub fn emission(&self, uv: Vec2) -> Spectrum {
        let emission = match &self.emissive_tex {
            Some(tex) => self.emissive * tex.sample(uv).truncate(),
            None => self.emissive,
        };
        match self.alpha_mode {
            AlphaMode::Opaque => emission,
            _ => emission * self.opacity_at(uv),
        }
    }

    /// Fraction of light a shadow ray keeps when crossing thin glass, `None` if the material
    /// is not thin glass. Partial coverage is not included, see `shadow_transmittance`.
    #[inline]
    pub fn glass_transmittance(&self, uv: Vec2) -> Option<Spectrum> {
        if !self.thin || self.transmission <= 0.0 {
            return None;
        }
        Some(self.base_color_at(uv) * (self.transmission * (1.0 - GLASS_F0) * (1.0 - self.metallic)))
    }

    /// Fraction of light a shadow ray keeps when crossing the surface, `None` if it is opaque:
    /// what goes through the uncovered part, plus what the glass lets through of the rest.
    #[inline]
    pub fn shadow_transmittance(&self, uv: Vec2) -> Option<Spectrum> {
        let glass = self.glass_transmittance(uv);
        let coverage = self.coverage(uv);
        if coverage >= 1.0 {
            return glass;
        }
        Some(Spectrum::splat(1.0 - coverage) + glass.unwrap_or(Spectrum::ZERO) * coverage)
    }

    /// Tangent space normal from the normal map, if any.
    #[inline]
    pub fn normal_at(&self, uv: Vec2) -> Option<Vec3> {
        let map = self.normal_tex.as_ref()?;
        let n = map.sample(uv).truncate() * 2.0 - Vec3::ONE;
        let mut xy = Vec2::new(n.x, n.y) * self.normal_scale;
        // The tangent frame follows the mesh UVs, while the x and y of the map follow the
        // transformed ones: a rotated map is turned back, so the bumps lean the right way.
        if let Some(t) = &map.transform {
            if t.matrix2.determinant() != 0.0 {
                let inverse = t.matrix2.inverse();
                xy = inverse.x_axis.normalize_or_zero() * xy.x + inverse.y_axis.normalize_or_zero() * xy.y;
            }
        }
        Some(Vec3::new(xy.x, xy.y, n.z))
    }

    /// Populate a scattering event, the equivalent of `MaterialBase::UpdateScatteringEvent`.
    ///
    /// `min_alpha` is a lower bound for the width of the specular lobes (the GGX alpha, the
    /// square of the roughness), zero for the material as authored. See
    /// `PathTracing::filter_glossy`.
    pub fn update_scattering_event(&self, se: &mut ScatteringEvent, uv: Vec2, min_alpha: f32) {
        let (basecolor, alpha) = self.base_color_and_alpha(uv);
        let coverage = if self.alpha_mode == AlphaMode::Blend { self.opacity_from_alpha(alpha) } else { 1.0 };
        // Partial coverage: the surface is there with probability `coverage`, otherwise light
        // goes straight through.
        if coverage < 1.0 {
            se.add_bxdf(Bxdf::Transparent(Spectrum::ONE), Spectrum::splat(1.0 - coverage), 1.0 - coverage);
        }
        let mut add = |bxdf: Bxdf, eval_weight: Spectrum, sample_weight: f32| {
            se.add_bxdf(bxdf, eval_weight * coverage, sample_weight * coverage)
        };

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
            self.add_surface(&mut add, disney, uv, min_alpha, 1.0);
            return;
        }

        // Thin glass: a blend between the opaque surface and a pane made of a straight-through
        // transmission plus a dielectric reflection, the way a window is authored in SORT with
        // a Transparent closure.
        self.add_surface(&mut add, disney, uv, min_alpha, 1.0 - glass);
        add(Bxdf::Transparent(basecolor), Spectrum::splat(glass * (1.0 - GLASS_F0)), glass * 0.85);
        add(
            Bxdf::MicrofacetReflection(MicrofacetReflection {
                r: Spectrum::ONE,
                dist: Ggx::new(roughness, roughness),
                fresnel: Fresnel::Dielectric { eta_i: 1.0, eta_t: self.ior },
            }),
            Spectrum::splat(glass),
            glass * 0.15,
        );
    }

    /// Add the surface proper, with weight `weight`: the Disney BRDF, or the coated one, or a
    /// cloth, with the sheen over it. At most 3 BXDFs.
    fn add_surface(
        &self,
        add: &mut impl FnMut(Bxdf, Spectrum, f32),
        disney: Disney,
        uv: Vec2,
        min_alpha: f32,
        weight: f32,
    ) {
        let solid_glass = disney.spec_trans > 0.0;
        let cloth = if solid_glass { None } else { self.cloth };
        let coat = if solid_glass || cloth.is_some() { 0.0 } else { self.coat_at(uv) };
        let sheen = self.sheen_color_at(uv);
        if cloth.is_none() && coat <= 0.0 && is_black(sheen) {
            add(Bxdf::Disney(disney), Spectrum::splat(weight), weight);
            return;
        }

        let sheen_max = sheen.max_element();
        let base = weight * (1.0 - sheen_max * SHEEN_ALBEDO);
        let basecolor = disney.basecolor;
        match cloth {
            Some(Cloth::Fabric { roughness }) => {
                add(Bxdf::Fabric(Fabric { base_color: basecolor, roughness }), Spectrum::splat(base), base);
            }
            Some(Cloth::Distribution { roughness, specular, specular_tint }) => {
                let bxdf = DistributionBrdf { base_color: basecolor, roughness, specular, specular_tint };
                add(Bxdf::DistributionBrdf(bxdf), Spectrum::splat(base), base);
            }
            None => {
                if coat > 0.0 {
                    let mut roughness = self.coat_roughness;
                    if let Some(tex) = &self.coat_roughness_tex {
                        roughness *= tex.sample(uv).y;
                    }
                    // the coat is blurred as the base is, see `filter_glossy`
                    if min_alpha > 0.0 {
                        roughness = roughness.max(min_alpha.sqrt());
                    }
                    // coat_color is exp(-2 sigma), the layer having a thickness of 1
                    let sigma = -0.5 * self.coat_color.clamp(Spectrum::splat(1e-4), Spectrum::ONE).ln();
                    let coated = Coat::new(disney, roughness, self.coat_ior, sigma);
                    add(Bxdf::Coat(coated), Spectrum::splat(base * coat), base * coat);
                }
                if coat < 1.0 {
                    add(Bxdf::Disney(disney), Spectrum::splat(base * (1.0 - coat)), base * (1.0 - coat));
                }
            }
        }

        if sheen_max > 0.0 {
            // SORT's distribution BRDF reduced to its cloth lobe, whose inverted Gaussian has
            // a width 1 - roughness^2: that width is the sheen roughness.
            let roughness = (1.0 - self.sheen_roughness_at(uv)).max(0.0).sqrt();
            let bxdf = DistributionBrdf { base_color: sheen, roughness, specular: 1.0, specular_tint: 1.0 };
            add(Bxdf::DistributionBrdf(bxdf), Spectrum::splat(weight), weight * sheen_max * SHEEN_ALBEDO);
        }
    }

    #[inline]
    fn coat_at(&self, uv: Vec2) -> f32 {
        let coat = match &self.coat_tex {
            Some(tex) if self.coat > 0.0 => self.coat * tex.sample(uv).x,
            _ => self.coat,
        };
        coat.clamp(0.0, 1.0)
    }

    #[inline]
    fn sheen_color_at(&self, uv: Vec2) -> Spectrum {
        let color = match &self.sheen_color_tex {
            Some(tex) if !is_black(self.sheen_color) => self.sheen_color * tex.sample(uv).truncate(),
            _ => self.sheen_color,
        };
        color.clamp(Spectrum::ZERO, Spectrum::ONE)
    }

    #[inline]
    fn sheen_roughness_at(&self, uv: Vec2) -> f32 {
        let roughness = match &self.sheen_roughness_tex {
            Some(tex) => self.sheen_roughness * tex.sample(uv).w,
            None => self.sheen_roughness,
        };
        roughness.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;
    use std::f32::consts::FRAC_PI_2;

    fn texture(data: Vec<[u8; 4]>, width: usize, srgb: bool) -> Arc<Texture> {
        let height = data.len() / width;
        Arc::new(Texture { width, height, data, srgb })
    }

    /// The example of the extension: offset [0, 1], rotation 90 degrees and scale 0.5 use the
    /// lower left quadrant of the image (v points down).
    #[test]
    fn khr_texture_transform_matches_the_specification_example() {
        let t = TextureMap::khr_texture_transform(Vec2::new(0.0, 1.0), FRAC_PI_2, Vec2::splat(0.5));
        for (uv, expected) in [
            (Vec2::new(0.0, 0.0), Vec2::new(0.0, 1.0)),
            (Vec2::new(1.0, 0.0), Vec2::new(0.0, 0.5)),
            (Vec2::new(0.0, 1.0), Vec2::new(0.5, 1.0)),
            (Vec2::new(1.0, 1.0), Vec2::new(0.5, 0.5)),
        ] {
            assert!((t.transform_point2(uv) - expected).length() < 1e-6, "{uv} -> {}", t.transform_point2(uv));
        }
    }

    /// A normal map rotated by 90 degrees leans its bumps along the other tangent axis.
    #[test]
    fn rotated_normal_map_turns_the_tangent_space_normal() {
        // a normal tilted towards +x in the map
        let tilted = texture(vec![[255, 128, 128, 255]], 1, false);
        let plain = Material { normal_tex: Some(tilted.clone().into()), ..Default::default() };
        let n = plain.normal_at(Vec2::splat(0.5)).unwrap();
        assert!((n.x - 1.0).abs() < 0.01 && n.y.abs() < 0.01, "{n}");

        let transform = TextureMap::khr_texture_transform(Vec2::ZERO, FRAC_PI_2, Vec2::splat(3.0));
        let rotated =
            Material { normal_tex: Some(TextureMap { texture: tilted, transform: Some(transform) }), ..Default::default() };
        // u' = v: moving along the mesh's v is moving along the map's x
        let n = rotated.normal_at(Vec2::splat(0.5)).unwrap();
        assert!(n.x.abs() < 0.01 && (n.y - 1.0).abs() < 0.01, "{n}");
    }

    #[test]
    fn opacity_follows_the_alpha_mode() {
        // alpha 255 on the left texel, 0 on the right one
        let half = texture(vec![[255, 255, 255, 255], [255, 255, 255, 0]], 2, true);
        let left = Vec2::new(0.25, 0.5);
        let right = Vec2::new(0.75, 0.5);

        let opaque = Material { base_color_tex: Some(half.clone().into()), ..Default::default() };
        assert_eq!((opaque.opacity_at(right), opaque.shadow_transmittance(right)), (1.0, None));
        assert!(!opaque.has_cutouts() && !opaque.lets_shadows_through());

        let mask = Material { alpha_mode: AlphaMode::Mask { cutoff: 0.5 }, ..opaque.clone() };
        assert_eq!((mask.opacity_at(left), mask.opacity_at(right)), (1.0, 0.0));
        assert!(mask.has_cutouts() && !mask.lets_shadows_through());

        let blend = Material { alpha_mode: AlphaMode::Blend, alpha: 0.5, ..opaque.clone() };
        assert_eq!((blend.opacity_at(left), blend.opacity_at(right)), (0.5, 0.0));
        assert_eq!(blend.shadow_transmittance(left), Some(Spectrum::splat(0.5)));
        assert!(!blend.has_cutouts() && blend.lets_shadows_through());
        // a fully opaque blended material is plain opaque
        let solid = Material { alpha_mode: AlphaMode::Blend, ..Default::default() };
        assert!(!solid.lets_shadows_through() && solid.shadow_transmittance(left).is_none());
    }

    /// Partially covering thin glass uses the most BXDFs a material produces: they must all
    /// fit (`ScatteringEvent::add_bxdf` asserts it in debug builds), and the light going
    /// straight through must be what shadow rays get.
    #[test]
    fn blended_thin_glass_fits_in_a_scattering_event() {
        let material = Material {
            base_color: Spectrum::ONE,
            transmission: 1.0,
            thin: true,
            alpha: 0.5,
            alpha_mode: AlphaMode::Blend,
            ..Default::default()
        };
        let mut se = ScatteringEvent::new(Vec3::Y, Vec3::X);
        material.update_scattering_event(&mut se, Vec2::ZERO, 0.0);
        let tr = material.shadow_transmittance(Vec2::ZERO).unwrap();
        assert!((tr - Spectrum::splat(0.5 + 0.5 * (1.0 - GLASS_F0))).abs().max_element() < 1e-6, "{tr}");
    }

    /// A partial coat and a sheen on partially covering thin glass take all the places.
    #[test]
    fn layered_blended_thin_glass_fits_in_a_scattering_event() {
        let material = Material {
            base_color: Spectrum::ONE,
            transmission: 0.5,
            thin: true,
            alpha: 0.5,
            alpha_mode: AlphaMode::Blend,
            coat: 0.5,
            sheen_color: Spectrum::splat(0.5),
            ..Default::default()
        };
        let mut se = ScatteringEvent::new(Vec3::Y, Vec3::X);
        material.update_scattering_event(&mut se, Vec2::ZERO, 0.0);
    }

    fn dir(theta_deg: f32, phi_deg: f32) -> Vec3 {
        spherical_vec(theta_deg.to_radians(), phi_deg.to_radians())
    }

    /// A coat, a sheen and partial coverage together: sampling the scattering event a
    /// material builds must agree with evaluating it, and with its pdf.
    #[test]
    fn layered_material_is_consistent() {
        let material = Material {
            base_color: Spectrum::new(0.8, 0.6, 0.4),
            roughness: 0.5,
            coat: 0.6,
            coat_roughness: 0.4,
            coat_color: Spectrum::new(0.9, 0.8, 0.6),
            sheen_color: Spectrum::new(0.6, 0.5, 0.4),
            sheen_roughness: 0.5,
            alpha: 0.7,
            alpha_mode: AlphaMode::Blend,
            ..Default::default()
        };
        let mut se = ScatteringEvent::new(Vec3::new(0.2, 0.9, 0.1), Vec3::X);
        material.update_scattering_event(&mut se, Vec2::ZERO, 0.0);

        let mut rng = Rng::new(17, 5);
        for wo in [dir(30.0, 60.0), dir(75.0, 10.0)] {
            let wo = se.local_to_world(wo);
            let n = 300_000;
            let (mut importance, mut uniform) = (Spectrum::ZERO, Spectrum::ZERO);
            for _ in 0..n {
                let bs = rng.pair();
                // Straight through the uncovered part is a delta, which uniform sampling never
                // finds. Samples of value zero are dropped too: the specular lobe of the Disney
                // BRDF may sample a direction below the surface, where its pdf is not zero
                // but ill-conditioned next to -wo.
                if let Some(s) = se.sample_bsdf(wo, bs, &mut rng).filter(|s| !s.delta && !is_black(s.f)) {
                    let pdf = se.pdf_bsdf(wo, s.wi);
                    assert!((pdf - s.pdf).abs() <= 1e-3 * s.pdf.max(1.0), "pdf {pdf} vs sampled {}", s.pdf);
                    importance += s.f / s.pdf;
                }
                let (u, v) = rng.pair();
                let wi = uniform_sample_sphere(u, v);
                uniform += se.evaluate_bsdf(wo, wi) / uniform_sphere_pdf();
            }
            let (importance, uniform) = (importance / n as f32, uniform / n as f32);
            assert!((importance - uniform).abs().max_element() < 0.02, "{importance:?} vs {uniform:?}");
            // the covered 70% reflects less than all of the light
            assert!(importance.max_element() < 0.7, "{importance:?}");
        }
    }

    /// The color of a coat tints the base seen through it, not the reflection of the coat.
    #[test]
    fn coat_color_tints_the_base_only() {
        let material = Material {
            base_color: Spectrum::ONE,
            specular: 0.0,
            roughness: 1.0,
            coat: 1.0,
            coat_roughness: 0.1,
            coat_color: Spectrum::new(1.0, 0.5, 0.25),
            ..Default::default()
        };
        let mut se = ScatteringEvent::new(Vec3::Y, Vec3::X);
        material.update_scattering_event(&mut se, Vec2::ZERO, 0.0);

        let wo = dir(40.0, 0.0);
        // the mirror direction: the reflection of the coat outweighs the base by far
        let mirror = se.evaluate_bsdf(wo, dir(40.0, 180.0));
        assert!(mirror.max_element() / mirror.min_element() < 1.01, "{mirror}");
        // elsewhere only the base is seen, through the tinted layer
        let base = se.evaluate_bsdf(wo, dir(20.0, 90.0));
        let (green, blue) = (base.y / base.x, base.z / base.x);
        assert!(green < 0.6 && blue < 0.8 * green, "{base}");
    }
}
