// This file is a part of SORT-rs, a Rust port of SORT (Simple Open Ray Tracing).

//! glTF 2.0 scene import.
//!
//! SORT reads a binary stream written by its Blender exporter. This port has no exporter, so
//! scenes come from glTF files instead: meshes, PBR materials, punctual lights and cameras.

use crate::camera::Camera;
use crate::light::Light;
use crate::material::{AlphaMode, Material, Texture, TextureMap};
use crate::math::*;
use crate::scene::SceneBuilder;
use glam::Affine2;
use gltf::json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// Camera description found in the file, independent of the output resolution.
#[derive(Clone, Debug)]
pub struct CameraDesc {
    pub eye: Vec3,
    pub forward: Vec3,
    pub up: Vec3,
    /// Vertical field of view in radians (a default for an orthographic camera).
    pub fov_y: f32,
    pub aspect_ratio: Option<f32>,
    /// Height of the view volume of an orthographic camera (glTF `ymag` is half of it),
    /// `None` for a perspective one.
    pub ortho_height: Option<f32>,
}

impl CameraDesc {
    pub fn to_camera(&self, width: u32, height: u32) -> Camera {
        let target = self.eye + self.forward;
        match self.ortho_height {
            Some(view_height) => Camera::orthographic(self.eye, target, self.up, view_height, width, height),
            None => Camera::look_at(self.eye, target, self.up, self.fov_y, width, height),
        }
    }
}

pub struct Loaded {
    pub builder: SceneBuilder,
    pub cameras: Vec<CameraDesc>,
}

struct TextureCache<'a> {
    document: &'a gltf::Document,
    images: &'a [gltf::image::Data],
    cache: HashMap<(usize, bool), Option<Arc<Texture>>>,
    /// Some texture asked for a texture coordinate set other than `TEXCOORD_0`, the only one
    /// loaded.
    other_uv_sets: bool,
}

impl TextureCache<'_> {
    /// The texture of a texture info with its `KHR_texture_transform`, if any.
    fn map(&mut self, info: gltf::texture::Info, srgb: bool) -> Option<TextureMap> {
        let transform = info.texture_transform();
        let tex_coord = transform.as_ref().and_then(|t| t.tex_coord()).unwrap_or(info.tex_coord());
        self.other_uv_sets |= tex_coord != 0;
        let transform = transform
            .map(|t| {
                TextureMap::khr_texture_transform(Vec2::from(t.offset()), t.rotation(), Vec2::from(t.scale()))
            })
            .filter(|t| *t != Affine2::IDENTITY);
        Some(TextureMap { texture: self.get(info.texture(), srgb)?, transform })
    }

    /// `map` for a texture info read from the raw JSON of an extension.
    fn map_json(&mut self, info: Option<&Value>, srgb: bool) -> Option<TextureMap> {
        let info = info?;
        let texture = self.document.textures().nth(info.get("index")?.as_u64()? as usize)?;
        let transform = info.get("extensions").and_then(|e| e.get("KHR_texture_transform"));
        let tex_coord = transform.and_then(|t| t.get("texCoord")).or(info.get("texCoord"));
        self.other_uv_sets |= tex_coord.and_then(Value::as_u64).unwrap_or(0) != 0;
        let transform = transform
            .map(|t| {
                let pair = |key: &str, default: Vec2| match t.get(key).and_then(Value::as_array).map(|a| a.as_slice()) {
                    Some([x, y]) => Vec2::new(number(Some(x), default.x), number(Some(y), default.y)),
                    _ => default,
                };
                TextureMap::khr_texture_transform(
                    pair("offset", Vec2::ZERO),
                    number(t.get("rotation"), 0.0),
                    pair("scale", Vec2::ONE),
                )
            })
            .filter(|t| *t != Affine2::IDENTITY);
        Some(TextureMap { texture: self.get(texture, srgb)?, transform })
    }

    fn get(&mut self, texture: gltf::Texture, srgb: bool) -> Option<Arc<Texture>> {
        let index = texture.source().index();
        let images = self.images;
        self.cache
            .entry((index, srgb))
            .or_insert_with(|| {
                let texture = images.get(index).and_then(|image| convert_image(image, srgb));
                if texture.is_none() {
                    eprintln!("warning: unsupported pixel format in image {index}, texture ignored");
                }
                texture.map(Arc::new)
            })
            .clone()
    }
}

fn convert_image(image: &gltf::image::Data, srgb: bool) -> Option<Texture> {
    use gltf::image::Format;
    // (channels, bytes per channel)
    let (channels, bytes) = match image.format {
        Format::R8 => (1, 1),
        Format::R8G8 => (2, 1),
        Format::R8G8B8 => (3, 1),
        Format::R8G8B8A8 => (4, 1),
        Format::R16 => (1, 2),
        Format::R16G16 => (2, 2),
        Format::R16G16B16 => (3, 2),
        Format::R16G16B16A16 => (4, 2),
        _ => return None,
    };
    let (width, height) = (image.width as usize, image.height as usize);
    if image.pixels.len() < width * height * channels * bytes {
        return None;
    }
    // 16-bit data is little endian, keep the most significant byte
    let channel = |pixel: usize, c: usize| image.pixels[(pixel * channels + c) * bytes + (bytes - 1)];
    let data = (0..width * height)
        .map(|i| match channels {
            1 => [channel(i, 0), channel(i, 0), channel(i, 0), 255],
            2 => [channel(i, 0), channel(i, 0), channel(i, 0), channel(i, 1)],
            3 => [channel(i, 0), channel(i, 1), channel(i, 2), 255],
            _ => [channel(i, 0), channel(i, 1), channel(i, 2), channel(i, 3)],
        })
        .collect();
    Some(Texture { width, height, data, srgb })
}

/// A number of an extension's JSON, `default` when it is missing.
fn number(value: Option<&Value>, default: f32) -> f32 {
    value.and_then(Value::as_f64).map_or(default, |v| v as f32)
}

/// A color of an extension's JSON, `default` when it is missing.
fn color(value: Option<&Value>, default: Spectrum) -> Spectrum {
    match value.and_then(Value::as_array).map(|a| a.as_slice()) {
        Some([r, g, b]) => Spectrum::new(number(Some(r), default.x), number(Some(g), default.y), number(Some(b), default.z)),
        _ => default,
    }
}

fn convert_material(material: &gltf::Material, textures: &mut TextureCache) -> Material {
    let pbr = material.pbr_metallic_roughness();
    let base = pbr.base_color_factor();
    let emissive_strength = material.emissive_strength().unwrap_or(1.0);
    let transmission = material.transmission().map_or(0.0, |t| t.transmission_factor());
    // Per the glTF specification a transmissive material without a volume is thin-walled.
    let thickness = material.volume().map_or(0.0, |v| v.thickness_factor());
    let alpha_mode = match material.alpha_mode() {
        gltf::material::AlphaMode::Opaque => AlphaMode::Opaque,
        gltf::material::AlphaMode::Mask => AlphaMode::Mask { cutoff: material.alpha_cutoff().unwrap_or(0.5) },
        gltf::material::AlphaMode::Blend => AlphaMode::Blend,
    };
    // this version of the gltf crate does not expose KHR_texture_transform on normal textures
    let normal_tex = material.normal_texture().and_then(|t| {
        textures.other_uv_sets |= t.tex_coord() != 0;
        textures.get(t.texture(), false).map(TextureMap::from)
    });

    // The crate has no types for these two, their JSON is read as it is. The clearcoat normal
    // texture is not used.
    let clearcoat = material.extension_value("KHR_materials_clearcoat");
    let clearcoat_field = |key: &str| clearcoat.and_then(|c| c.get(key));
    let sheen = material.extension_value("KHR_materials_sheen");
    let sheen_field = |key: &str| sheen.and_then(|s| s.get(key));

    Material {
        name: material.name().unwrap_or("").to_string(),
        base_color: Vec3::new(base[0], base[1], base[2]),
        base_color_tex: pbr.base_color_texture().and_then(|t| textures.map(t, true)),
        alpha: base[3],
        alpha_mode,
        metallic: pbr.metallic_factor(),
        roughness: pbr.roughness_factor(),
        metallic_roughness_tex: pbr.metallic_roughness_texture().and_then(|t| textures.map(t, false)),
        normal_tex,
        normal_scale: material.normal_texture().map_or(1.0, |t| t.scale()),
        emissive: Vec3::from(material.emissive_factor()) * emissive_strength,
        emissive_tex: material.emissive_texture().and_then(|t| textures.map(t, true)),
        transmission,
        thin: transmission > 0.0 && thickness <= 0.0,
        ior: material.ior().unwrap_or(1.5),
        // the index of refraction of the clearcoat is 1.5 in glTF, the coat's default
        coat: number(clearcoat_field("clearcoatFactor"), 0.0),
        coat_tex: textures.map_json(clearcoat_field("clearcoatTexture"), false),
        coat_roughness: number(clearcoat_field("clearcoatRoughnessFactor"), 0.0),
        coat_roughness_tex: textures.map_json(clearcoat_field("clearcoatRoughnessTexture"), false),
        sheen_color: color(sheen_field("sheenColorFactor"), Spectrum::ZERO),
        sheen_color_tex: textures.map_json(sheen_field("sheenColorTexture"), true),
        sheen_roughness: number(sheen_field("sheenRoughnessFactor"), 0.0),
        sheen_roughness_tex: textures.map_json(sheen_field("sheenRoughnessTexture"), false),
        double_sided: material.double_sided(),
        // a material whose name starts with "portal" marks an opening for sky light
        portal: material.name().is_some_and(|n| n.to_ascii_lowercase().starts_with("portal")),
        ..Default::default()
    }
}

struct Importer<'a> {
    buffers: &'a [gltf::buffer::Data],
    builder: SceneBuilder,
    cameras: Vec<CameraDesc>,
    /// Index of the material used by primitives without one.
    default_material: Option<u32>,
    skipped_primitives: usize,
}

impl Importer<'_> {
    fn visit(&mut self, node: gltf::Node, parent: Mat4) {
        let world = parent * Mat4::from_cols_array_2d(&node.transform().matrix());

        if let Some(mesh) = node.mesh() {
            for primitive in mesh.primitives() {
                self.add_primitive(&primitive, world);
            }
        }

        if let Some(camera) = node.camera() {
            use gltf::camera::Projection;
            let (fov_y, aspect_ratio, ortho_height) = match camera.projection() {
                Projection::Perspective(p) => (p.yfov(), p.aspect_ratio(), None),
                Projection::Orthographic(o) => {
                    let aspect = (o.ymag() != 0.0).then(|| (o.xmag() / o.ymag()).abs());
                    // the field of view only matters if the projection is switched to perspective
                    (40f32.to_radians(), aspect, Some(2.0 * o.ymag().abs()))
                }
            };
            self.cameras.push(CameraDesc {
                eye: world.transform_point3(Vec3::ZERO),
                forward: world.transform_vector3(-Vec3::Z).normalize_or_zero(),
                up: world.transform_vector3(Vec3::Y).normalize_or_zero(),
                fov_y,
                aspect_ratio,
                ortho_height,
            });
        }

        if let Some(light) = node.light() {
            use gltf::khr_lights_punctual::Kind;
            let intensity = Vec3::from(light.color()) * light.intensity();
            let pos = world.transform_point3(Vec3::ZERO);
            let dir = world.transform_vector3(-Vec3::Z).normalize_or_zero();
            self.builder.add_light(match light.kind() {
                Kind::Directional => Light::Distant { dir, intensity },
                Kind::Point => Light::Point { pos, intensity },
                Kind::Spot { inner_cone_angle, outer_cone_angle } => Light::Spot {
                    pos,
                    dir,
                    cos_total_range: outer_cone_angle.cos(),
                    cos_falloff_start: inner_cone_angle.cos(),
                    intensity,
                },
            });
        }

        for child in node.children() {
            self.visit(child, world);
        }
    }

    fn add_primitive(&mut self, primitive: &gltf::Primitive, world: Mat4) {
        if primitive.mode() != gltf::mesh::Mode::Triangles {
            self.skipped_primitives += 1;
            return;
        }
        let buffers = self.buffers;
        let reader = primitive.reader(|buffer| buffers.get(buffer.index()).map(|data| &data.0[..]));
        let Some(positions) = reader.read_positions() else {
            self.skipped_primitives += 1;
            return;
        };

        let normal_matrix = glam::Mat3::from_mat4(world).inverse().transpose();
        let positions: Vec<Vec3> = positions.map(|p| world.transform_point3(Vec3::from(p))).collect();
        let normals: Option<Vec<Vec3>> = reader
            .read_normals()
            .map(|it| it.map(|n| (normal_matrix * Vec3::from(n)).normalize_or_zero()).collect());
        let uvs: Option<Vec<Vec2>> = reader.read_tex_coords(0).map(|it| it.into_f32().map(Vec2::from).collect());
        let tangents: Option<Vec<Vec4>> = reader.read_tangents().map(|it| {
            it.map(|t| world.transform_vector3(Vec3::new(t[0], t[1], t[2])).normalize_or_zero().extend(t[3]))
                .collect()
        });

        let flat: Vec<u32> = match reader.read_indices() {
            Some(indices) => indices.into_u32().collect(),
            None => (0..positions.len() as u32).collect(),
        };
        // a mirrored transform flips the winding order
        let flip = world.determinant() < 0.0;
        let indices: Vec<[u32; 3]> =
            flat.chunks_exact(3).map(|c| if flip { [c[0], c[2], c[1]] } else { [c[0], c[1], c[2]] }).collect();

        let material = match primitive.material().index() {
            Some(index) => index as u32,
            None => *self
                .default_material
                .get_or_insert_with(|| self.builder.add_material(Material { name: "default".into(), ..Default::default() })),
        };
        self.builder.add_mesh(&positions, normals.as_deref(), uvs.as_deref(), tangents.as_deref(), &indices, material);
    }
}

/// Load a `.gltf` or `.glb` file.
pub fn load(path: &Path) -> Result<Loaded, gltf::Error> {
    let (document, buffers, images) = gltf::import(path)?;

    let mut builder = SceneBuilder::new();
    let mut textures =
        TextureCache { document: &document, images: &images, cache: HashMap::new(), other_uv_sets: false };
    for material in document.materials() {
        builder.add_material(convert_material(&material, &mut textures));
    }
    if textures.other_uv_sets {
        eprintln!(
            "warning: {}: some textures use a texture coordinate set other than TEXCOORD_0, \
             which is the only one loaded; they are mapped with TEXCOORD_0",
            path.display()
        );
    }

    let mut importer =
        Importer { buffers: &buffers, builder, cameras: Vec::new(), default_material: None, skipped_primitives: 0 };
    let scenes: Vec<gltf::Scene> = match document.default_scene() {
        Some(scene) => vec![scene],
        None => document.scenes().collect(),
    };
    for scene in scenes {
        for node in scene.nodes() {
            importer.visit(node, Mat4::IDENTITY);
        }
    }
    if importer.skipped_primitives > 0 {
        eprintln!(
            "warning: skipped {} primitive(s) that are not triangle lists or have no positions",
            importer.skipped_primitives
        );
    }

    Ok(Loaded { builder: importer.builder, cameras: importer.cameras })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::light::Light;

    fn base64(data: &[u8]) -> String {
        const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    /// A quad with a thin glass material, a camera and a sun, all under transformed nodes.
    #[test]
    fn loads_a_minimal_scene() {
        let mut buffer = Vec::new();
        for p in [[-1.0f32, 0.0, 1.0], [1.0, 0.0, 1.0], [1.0, 0.0, -1.0], [-1.0, 0.0, -1.0]] {
            for c in p {
                buffer.extend_from_slice(&c.to_le_bytes());
            }
        }
        for i in [0u16, 1, 2, 0, 2, 3] {
            buffer.extend_from_slice(&i.to_le_bytes());
        }
        let json = format!(
            r#"{{
  "asset": {{ "version": "2.0" }},
  "extensionsUsed": ["KHR_lights_punctual", "KHR_materials_transmission"],
  "extensions": {{ "KHR_lights_punctual": {{ "lights": [
      {{ "type": "directional", "color": [1.0, 0.5, 0.25], "intensity": 4.0 }} ] }} }},
  "scene": 0,
  "scenes": [{{ "nodes": [0, 1, 2] }}],
  "nodes": [
    {{ "mesh": 0, "translation": [0.0, 2.0, 0.0], "scale": [3.0, 1.0, 3.0] }},
    {{ "camera": 0, "translation": [0.0, 1.0, 5.0] }},
    {{ "extensions": {{ "KHR_lights_punctual": {{ "light": 0 }} }},
       "rotation": [-0.7071068, 0.0, 0.0, 0.7071068] }}
  ],
  "cameras": [{{ "type": "perspective", "perspective": {{ "yfov": 0.6, "znear": 0.1, "aspectRatio": 1.5 }} }}],
  "materials": [{{
    "name": "glass",
    "pbrMetallicRoughness": {{ "baseColorFactor": [0.9, 1.0, 0.8, 1.0], "roughnessFactor": 0.1, "metallicFactor": 0.0 }},
    "extensions": {{ "KHR_materials_transmission": {{ "transmissionFactor": 1.0 }} }}
  }}],
  "meshes": [{{ "primitives": [{{ "attributes": {{ "POSITION": 0 }}, "indices": 1, "material": 0 }}] }}],
  "accessors": [
    {{ "bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3", "min": [-1.0, 0.0, -1.0], "max": [1.0, 0.0, 1.0] }},
    {{ "bufferView": 1, "componentType": 5123, "count": 6, "type": "SCALAR" }}
  ],
  "bufferViews": [
    {{ "buffer": 0, "byteOffset": 0, "byteLength": 48 }},
    {{ "buffer": 0, "byteOffset": 48, "byteLength": 12 }}
  ],
  "buffers": [{{ "byteLength": 60, "uri": "data:application/octet-stream;base64,{}" }}]
}}"#,
            base64(&buffer)
        );
        let path = std::env::temp_dir().join(format!("sort_rs_test_{}.gltf", std::process::id()));
        std::fs::write(&path, json).unwrap();
        let loaded = load(&path);
        std::fs::remove_file(&path).ok();
        let loaded = loaded.expect("the file must load");

        assert_eq!(loaded.cameras.len(), 1);
        let camera = &loaded.cameras[0];
        assert!((camera.eye - Vec3::new(0.0, 1.0, 5.0)).length() < 1e-5);
        assert!((camera.forward - -Vec3::Z).length() < 1e-5);
        assert_eq!(camera.aspect_ratio, Some(1.5));

        let material = &loaded.builder.materials[0];
        assert_eq!(material.name, "glass");
        assert!(material.thin && material.transmission == 1.0);
        assert!((material.base_color - Vec3::new(0.9, 1.0, 0.8)).length() < 1e-5);

        // the light node is rotated to point straight down
        match &loaded.builder.lights[..] {
            [Light::Distant { dir, intensity }] => {
                assert!((*dir - -Vec3::Y).length() < 1e-4, "{dir:?}");
                assert!((*intensity - Vec3::new(4.0, 2.0, 1.0)).length() < 1e-5);
            }
            _ => panic!("expected exactly one distant light"),
        }

        let scene = loaded.builder.build();
        assert_eq!(scene.triangle_count(), 2);
        // the quad is scaled by 3 and lifted to y = 2, facing up
        let hit = scene.intersect(&Ray::new(Vec3::new(2.5, 5.0, -2.5), -Vec3::Y, 0.0, f32::MAX)).unwrap();
        assert!((hit.t - 3.0).abs() < 1e-4 && (hit.ng - Vec3::Y).length() < 1e-5);
        assert!(scene.intersect(&Ray::new(Vec3::new(3.5, 5.0, 0.0), -Vec3::Y, 0.0, f32::MAX)).is_none());
        // a shadow ray crosses the thin glass attenuated, not blocked
        let through = scene.transmittance(&Ray::new(Vec3::new(0.0, 5.0, 0.0), -Vec3::Y, 0.0, 10.0));
        assert!(through.y > 0.9 && through.y < 1.0, "{through:?}");
    }

    /// Alpha modes and a texture transform on the base color texture.
    #[test]
    fn loads_alpha_modes_and_texture_transforms() {
        // a 2x2 image, blue in the lower left quadrant (v points down)
        let dir = std::env::temp_dir();
        let stem = format!("sort_rs_alpha_test_{}", std::process::id());
        let png = dir.join(format!("{stem}.png"));
        let pixels = [[255u8, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [255, 255, 255, 0]];
        image::RgbaImage::from_fn(2, 2, |x, y| image::Rgba(pixels[(y * 2 + x) as usize])).save(&png).unwrap();

        let json = format!(
            r#"{{
  "asset": {{ "version": "2.0" }},
  "extensionsUsed": ["KHR_texture_transform"],
  "images": [{{ "uri": "{stem}.png" }}],
  "textures": [{{ "source": 0 }}],
  "materials": [
    {{ "name": "leaf", "alphaMode": "MASK", "alphaCutoff": 0.3,
       "pbrMetallicRoughness": {{ "baseColorTexture": {{ "index": 0 }} }} }},
    {{ "name": "curtain", "alphaMode": "BLEND",
       "pbrMetallicRoughness": {{ "baseColorFactor": [1.0, 1.0, 1.0, 0.4] }} }},
    {{ "name": "tiles", "pbrMetallicRoughness": {{ "baseColorTexture": {{ "index": 0,
       "extensions": {{ "KHR_texture_transform": {{ "offset": [0, 1], "rotation": 1.57079632679, "scale": [0.5, 0.5] }} }} }} }} }},
    {{ "name": "other set", "pbrMetallicRoughness": {{ "baseColorTexture": {{ "index": 0, "texCoord": 1 }} }} }}
  ]
}}"#
        );
        let path = dir.join(format!("{stem}.gltf"));
        std::fs::write(&path, json).unwrap();
        let loaded = load(&path);
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&png).ok();
        let materials = loaded.expect("the file must load").builder.materials;

        let [leaf, curtain, tiles, other] = &materials[..] else {
            panic!("expected four materials");
        };
        assert_eq!(leaf.alpha_mode, AlphaMode::Mask { cutoff: 0.3 });
        assert!(leaf.has_cutouts());
        // the alpha of the texture: a hole in the lower right texel only
        assert_eq!(leaf.opacity_at(Vec2::new(0.25, 0.25)), 1.0);
        assert_eq!(leaf.opacity_at(Vec2::new(0.75, 0.75)), 0.0);

        assert_eq!((curtain.alpha_mode, curtain.alpha), (AlphaMode::Blend, 0.4));
        assert!((curtain.shadow_transmittance(Vec2::ZERO).unwrap() - Spectrum::splat(0.6)).abs().max_element() < 1e-6);

        assert_eq!(tiles.alpha_mode, AlphaMode::Opaque);
        let map = tiles.base_color_tex.as_ref().unwrap();
        assert!(map.transform.is_some());
        // the middle of the mesh UVs is the middle of the lower left quadrant
        assert!((tiles.base_color_at(Vec2::splat(0.5)) - Vec3::new(0.0, 0.0, 1.0)).abs().max_element() < 1e-5);
        assert!(other.base_color_tex.as_ref().unwrap().transform.is_none());
    }

    /// `KHR_materials_clearcoat` and `KHR_materials_sheen`, read from the raw JSON.
    #[test]
    fn loads_clearcoat_and_sheen() {
        let dir = std::env::temp_dir();
        let stem = format!("sort_rs_coat_test_{}", std::process::id());
        let png = dir.join(format!("{stem}.png"));
        // half red on the left, a quarter on the right; alpha 1 and 0.5
        let pixels = [[255u8, 0, 0, 255], [64, 0, 0, 128]];
        image::RgbaImage::from_fn(2, 1, |x, _| image::Rgba(pixels[x as usize])).save(&png).unwrap();

        let material = |required: &str| {
            format!(
                r#"{{
  "asset": {{ "version": "2.0" }},
  "extensionsUsed": ["KHR_materials_clearcoat", "KHR_materials_sheen", "KHR_texture_transform"]{required},
  "images": [{{ "uri": "{stem}.png" }}],
  "textures": [{{ "source": 0 }}],
  "materials": [
    {{ "name": "velvet sofa with lacquered legs",
       "extensions": {{
         "KHR_materials_clearcoat": {{ "clearcoatFactor": 0.8, "clearcoatRoughnessFactor": 0.25,
            "clearcoatTexture": {{ "index": 0,
               "extensions": {{ "KHR_texture_transform": {{ "offset": [0.5, 0.0] }} }} }} }},
         "KHR_materials_sheen": {{ "sheenColorFactor": [1.0, 0.5, 0.25], "sheenRoughnessFactor": 0.6,
            "sheenRoughnessTexture": {{ "index": 0 }} }}
       }} }},
    {{ "name": "plain" }}
  ]
}}"#
            )
        };
        let path = dir.join(format!("{stem}.gltf"));
        std::fs::write(&path, material("")).unwrap();
        let loaded = load(&path);
        // the crate rejects the files that require an extension it does not know
        std::fs::write(&path, material(r#", "extensionsRequired": ["KHR_materials_clearcoat"]"#)).unwrap();
        let required = load(&path);
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&png).ok();
        assert!(required.is_err());
        let materials = loaded.expect("the file must load").builder.materials;

        let [sofa, plain] = &materials[..] else {
            panic!("expected two materials");
        };
        assert_eq!((sofa.coat, sofa.coat_roughness, sofa.coat_ior), (0.8, 0.25, 1.5));
        assert_eq!(sofa.sheen_color, Spectrum::new(1.0, 0.5, 0.25));
        assert_eq!(sofa.sheen_roughness, 0.6);
        assert!(sofa.coat_roughness_tex.is_none() && sofa.sheen_color_tex.is_none());
        // the coat texture is shifted by half its width: the left half of the mesh UVs reads
        // the right texel (red 64, linear), and the other way round
        let coat_tex = sofa.coat_tex.as_ref().unwrap();
        assert!(coat_tex.transform.is_some());
        assert!((coat_tex.sample(Vec2::new(0.25, 0.5)).x - 64.0 / 255.0).abs() < 1e-5);
        assert!((coat_tex.sample(Vec2::new(0.75, 0.5)).x - 1.0).abs() < 1e-5);
        let roughness_tex = sofa.sheen_roughness_tex.as_ref().unwrap();
        assert!((roughness_tex.sample(Vec2::new(0.75, 0.5)).w - 128.0 / 255.0).abs() < 1e-5);

        assert_eq!((plain.coat, plain.sheen_color), (0.0, Spectrum::ZERO));
        assert!(plain.coat_tex.is_none() && plain.sheen_roughness_tex.is_none());
    }
}
