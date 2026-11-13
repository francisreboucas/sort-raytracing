//! Coats, sheen and cloth: a row of spheres on a varnished floor, in the sun.
//!
//! From left to right: red plastic, the same plastic under a clear coat, blue velvet (a sheen
//! over a diffuse base), SORT's DreamWorks fabric and its distribution-based cloth. In front,
//! a plank of bare wood next to a varnished one.
//!
//! `cargo run --release --example materials -- [output.png] [samples per pixel]`

use sort_raytracing::camera::Camera;
use sort_raytracing::light::{Light, Sky};
use sort_raytracing::material::{Cloth, Material};
use sort_raytracing::math::{Vec2, Vec3};
use sort_raytracing::render::{render, save_image, RenderSettings, ToneMap};
use sort_raytracing::scene::SceneBuilder;
use std::path::Path;

/// A UV sphere: U goes around the vertical axis, which is the direction of the threads of
/// the fabric.
fn add_sphere(builder: &mut SceneBuilder, center: Vec3, radius: f32, material: u32) {
    let (rings, segments) = (48u32, 96u32);
    let (mut positions, mut normals, mut uvs) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..=rings {
        for j in 0..=segments {
            let (u, v) = (j as f32 / segments as f32, i as f32 / rings as f32);
            let (theta, phi) = (v * std::f32::consts::PI, u * std::f32::consts::TAU);
            let n = Vec3::new(theta.sin() * phi.cos(), theta.cos(), theta.sin() * phi.sin());
            positions.push(center + n * radius);
            normals.push(n);
            uvs.push(Vec2::new(u, v));
        }
    }
    let index = |i: u32, j: u32| i * (segments + 1) + j;
    let mut triangles = Vec::new();
    for i in 0..rings {
        for j in 0..segments {
            // counter-clockwise seen from outside
            triangles.push([index(i, j), index(i + 1, j + 1), index(i + 1, j)]);
            triangles.push([index(i, j), index(i, j + 1), index(i + 1, j + 1)]);
        }
    }
    builder.add_mesh(&positions, Some(&normals), Some(&uvs), None, &triangles, material);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let output = args.next().unwrap_or_else(|| "materials.png".into());
    let spp = args.next().map(|s| s.parse()).transpose()?.unwrap_or(128);

    let mut builder = SceneBuilder::new();
    let p = Vec3::new;

    let wood = Material { base_color: p(0.42, 0.25, 0.13), roughness: 0.7, ..Default::default() };
    // a slightly amber varnish
    let varnish = Material { coat: 1.0, coat_roughness: 0.05, coat_color: p(0.97, 0.9, 0.78), ..wood.clone() };
    let plastic = Material { base_color: p(0.6, 0.06, 0.04), roughness: 0.6, ..Default::default() };
    let materials = [
        plastic.clone(),
        Material { coat: 1.0, coat_roughness: 0.05, ..plastic },
        Material {
            base_color: p(0.05, 0.08, 0.3),
            roughness: 1.0,
            specular: 0.0,
            sheen_color: p(0.3, 0.35, 0.55),
            sheen_roughness: 0.3,
            ..Default::default()
        },
        Material { base_color: p(0.75, 0.55, 0.15), cloth: Some(Cloth::Fabric { roughness: 0.6 }), ..Default::default() },
        Material {
            base_color: p(0.35, 0.05, 0.2),
            cloth: Some(Cloth::Distribution { roughness: 0.8, specular: 0.6, specular_tint: 0.7 }),
            ..Default::default()
        },
    ];

    let floor = builder.add_material(varnish.clone());
    builder.add_quad([p(-20.0, 0.0, 20.0), p(20.0, 0.0, 20.0), p(20.0, 0.0, -20.0), p(-20.0, 0.0, -20.0)], floor);
    for (k, material) in materials.into_iter().enumerate() {
        let id = builder.add_material(material);
        add_sphere(&mut builder, p(-2.0 + k as f32, 0.4, 0.0), 0.4, id);
    }
    let bare = builder.add_material(wood);
    let varnished = builder.add_material(varnish);
    builder.add_box(p(-1.3, 0.0, 0.8), p(-0.1, 0.06, 1.2), bare);
    builder.add_box(p(0.1, 0.0, 0.8), p(1.3, 0.06, 1.2), varnished);

    let to_sun = p(-0.5, 0.7, 0.6).normalize();
    builder.add_light(Light::Distant { dir: -to_sun, intensity: p(6.0, 5.6, 5.0) });
    builder.add_light(Light::Sky(Sky { intensity: p(0.5, 0.65, 0.9), env: None, rotation: 0.0 }));

    let scene = builder.build();
    let camera = Camera::look_at(p(0.0, 1.5, 3.6), p(0.0, 0.3, 0.0), Vec3::Y, 45f32.to_radians(), 800, 400);
    let settings = RenderSettings { samples_per_pixel: spp, max_depth: 8, ..Default::default() };
    let pixels = render(&scene, &camera, &settings, |_fraction_done| {});
    save_image(Path::new(&output), &pixels, camera.width, camera.height, 1.0, ToneMap::Aces)?;
    println!("wrote {output}");
    Ok(())
}
