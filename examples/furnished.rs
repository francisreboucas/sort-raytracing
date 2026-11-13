//! Put a glTF model in a room with a window and render it: the loader and the scene builder
//! used together.
//!
//! `cargo run --release --example furnished -- assets/models/furniture_set.glb [out.png] [spp] [height] [side]`
//!
//! `height` is how tall the model is in meters (1.7 by default), which sets the scale. `side`
//! is where the camera stands, `+x` (the default) or `-x`: the side the model's front is on.

use sort_rs::camera::Camera;
use sort_rs::gltf_loader;
use sort_rs::light::{Light, Sky};
use sort_rs::material::Material;
use sort_rs::math::Vec3;
use sort_rs::render::{render, save_image, RenderSettings, ToneMap};
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let model = args.next().ok_or("usage: furnished <model.glb> [out.png] [spp] [height] [side]")?;
    let output = args.next().unwrap_or_else(|| "furnished.png".into());
    let spp: u32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(256);
    let height: f32 = args.next().and_then(|a| a.parse().ok()).filter(|h| *h > 0.0).unwrap_or(1.7);
    let from_minus_x = match args.next().as_deref() {
        None | Some("+x") => false,
        Some("-x") => true,
        Some(other) => return Err(format!("unknown side '{other}', expected +x or -x").into()),
    };

    let mut b = gltf_loader::load(Path::new(&model))?.builder;
    b.lights.clear();
    let bounds = b.bounds();
    // The room is laid out for a camera on the +x side of the model. For the other side it is
    // mirrored across the middle of the model, which itself stays where it is.
    let mirror = bounds.min.x + bounds.max.x;
    let p = |x: f32, y: f32, z: f32| Vec3::new(if from_minus_x { mirror - x } else { x }, y, z);

    let plaster = b.add_material(Material { base_color: Vec3::new(0.84, 0.82, 0.78), roughness: 0.9, ..Default::default() });
    let floor = b.add_material(Material { base_color: Vec3::new(0.45, 0.33, 0.22), roughness: 0.45, ..Default::default() });
    let glass = b.add_material(Material { base_color: Vec3::new(0.95, 0.98, 0.97), transmission: 1.0, thin: true, roughness: 0.0, ..Default::default() });

    // Models come in any unit. The room is sized after the model's height: `m` is one meter.
    let size = bounds.max - bounds.min;
    let m = size.y / height;
    println!("model: {:.2} x {:.2} x {:.2} units, 1 m taken as {m:.3} units", size.x, size.y, size.z);

    // A room around the model, standing on the floor and 2.8 m high. The camera looks along
    // -x, with a window along the wall on its left.
    let (x0, x1) = (bounds.min.x - 0.5 * m, bounds.max.x + 2.8 * m);
    let (z0, z1) = (bounds.min.z - 0.8 * m, bounds.max.z + 0.8 * m);
    let (y0, y1) = (bounds.min.y, bounds.min.y + 2.8 * m);
    b.add_quad([p(x0, y0, z1), p(x1, y0, z1), p(x1, y0, z0), p(x0, y0, z0)], floor);
    b.add_quad([p(x0, y1, z0), p(x1, y1, z0), p(x1, y1, z1), p(x0, y1, z1)], plaster);
    b.add_quad([p(x0, y0, z0), p(x1, y0, z0), p(x1, y1, z0), p(x0, y1, z0)], plaster);
    b.add_quad([p(x0, y0, z1), p(x0, y0, z0), p(x0, y1, z0), p(x0, y1, z1)], plaster);
    b.add_quad([p(x1, y0, z0), p(x1, y0, z1), p(x1, y1, z1), p(x1, y1, z0)], plaster);

    // the wall at z1 has a window from 0.4 m to 2.3 m high, set back 0.7 m from each end
    let (wx0, wx1, wy0, wy1) = (x0 + 0.7 * m, x1 - 0.7 * m, y0 + 0.4 * m, y0 + 2.3 * m);
    let mut wall = |xa: f32, xb: f32, ya: f32, yb: f32, material: u32| {
        b.add_quad([p(xb, ya, z1), p(xa, ya, z1), p(xa, yb, z1), p(xb, yb, z1)], material);
    };
    wall(x0, wx0, y0, y1, plaster);
    wall(wx1, x1, y0, y1, plaster);
    wall(wx0, wx1, y0, wy0, plaster);
    wall(wx0, wx1, wy1, y1, plaster);
    wall(wx0, wx1, wy0, wy1, glass);

    // the sun rakes across the room through the window
    let to_sun = Vec3::new(if from_minus_x { -0.2 } else { 0.2 }, 0.75, 1.0).normalize();
    b.add_light(Light::Distant { dir: -to_sun, intensity: Vec3::new(20.0, 18.0, 15.0) });
    b.add_light(Light::Sky(Sky { intensity: Vec3::new(1.2, 1.6, 2.2), env: None, rotation: 0.0 }));
    let scene = b.build();

    let center = bounds.center();
    let eye = p(x1 - 0.3 * m, y0 + 1.45 * m, center.z + 0.9 * m);
    let target = p(center.x, y0 + 0.75 * m, center.z);
    let camera = Camera::look_at(eye, target, Vec3::Y, 50f32.to_radians(), 960, 540);

    let settings = RenderSettings { samples_per_pixel: spp, max_depth: 8, ..Default::default() };
    let pixels = render(&scene, &camera, &settings, |_| {});
    save_image(Path::new(&output), &pixels, camera.width, camera.height, 0.6, ToneMap::Aces)?;
    println!("wrote {output} ({} triangles)", scene.triangle_count());
    Ok(())
}
