//! Using SORT-rs as a library: build a scene in code, render it and save the image.
//!
//! `cargo run --release --example library`

use sort_raytracing::camera::Camera;
use sort_raytracing::light::{Light, Sky};
use sort_raytracing::material::Material;
use sort_raytracing::math::Vec3;
use sort_raytracing::render::{render, save_image, RenderSettings, ToneMap};
use sort_raytracing::scene::SceneBuilder;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut builder = SceneBuilder::new();
    let p = Vec3::new;

    // materials
    let plaster = builder.add_material(Material { base_color: p(0.8, 0.78, 0.74), roughness: 0.9, ..Default::default() });
    let wood = builder.add_material(Material { base_color: p(0.45, 0.3, 0.18), roughness: 0.4, ..Default::default() });
    let steel = builder.add_material(Material { base_color: Vec3::splat(0.9), metallic: 1.0, roughness: 0.25, ..Default::default() });

    // A room of 4 x 3 x 4 meters, Y up: floor, ceiling and four walls, one of them with a
    // doorway. Quads are given counter-clockwise as seen from inside the room.
    builder.add_quad([p(-2.0, 0.0, 2.0), p(2.0, 0.0, 2.0), p(2.0, 0.0, -2.0), p(-2.0, 0.0, -2.0)], wood);
    builder.add_quad([p(-2.0, 3.0, -2.0), p(2.0, 3.0, -2.0), p(2.0, 3.0, 2.0), p(-2.0, 3.0, 2.0)], plaster);
    builder.add_quad([p(-2.0, 0.0, -2.0), p(2.0, 0.0, -2.0), p(2.0, 3.0, -2.0), p(-2.0, 3.0, -2.0)], plaster);
    builder.add_quad([p(2.0, 0.0, 2.0), p(-2.0, 0.0, 2.0), p(-2.0, 3.0, 2.0), p(2.0, 3.0, 2.0)], plaster);
    builder.add_quad([p(2.0, 0.0, -2.0), p(2.0, 0.0, 2.0), p(2.0, 3.0, 2.0), p(2.0, 3.0, -2.0)], plaster);
    // the wall at x = -2, around a doorway 1 m wide and 2.1 m high
    builder.add_quad([p(-2.0, 0.0, 2.0), p(-2.0, 0.0, 0.5), p(-2.0, 3.0, 0.5), p(-2.0, 3.0, 2.0)], plaster);
    builder.add_quad([p(-2.0, 0.0, -0.5), p(-2.0, 0.0, -2.0), p(-2.0, 3.0, -2.0), p(-2.0, 3.0, -0.5)], plaster);
    builder.add_quad([p(-2.0, 2.1, 0.5), p(-2.0, 2.1, -0.5), p(-2.0, 3.0, -0.5), p(-2.0, 3.0, 0.5)], plaster);

    // Tell the renderer where daylight comes in. Nothing is drawn for a portal; without it the
    // image is the same, only much noisier.
    builder.add_portal([p(-2.0, 0.0, 0.5), p(-2.0, 0.0, -0.5), p(-2.0, 2.1, -0.5), p(-2.0, 2.1, 0.5)]);

    builder.add_box(p(0.1, 0.0, -1.7), p(0.9, 0.8, -0.9), steel);

    // a low sun shining through the doorway, and a blue sky
    let to_sun = p(-1.0, 0.45, 0.2).normalize();
    builder.add_light(Light::Distant { dir: -to_sun, intensity: p(20.0, 18.0, 15.0) });
    builder.add_light(Light::Sky(Sky { intensity: p(1.2, 1.6, 2.2), env: None, rotation: 0.0 }));

    let scene = builder.build();
    let camera = Camera::look_at(p(1.6, 1.5, 1.7), p(-1.5, 0.9, -0.6), Vec3::Y, 60f32.to_radians(), 640, 360);

    let settings = RenderSettings { samples_per_pixel: 256, max_depth: 8, ..Default::default() };
    let pixels = render(&scene, &camera, &settings, |_fraction_done| {});
    save_image(Path::new("library.png"), &pixels, camera.width, camera.height, 1.0, ToneMap::Aces)?;
    println!("wrote library.png");
    Ok(())
}
