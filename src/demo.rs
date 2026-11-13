// This file is a part of SORT-rs, a Rust port of SORT (Simple Open Ray Tracing).

//! A small built-in interior scene, handy to try the renderer without any asset.

use crate::camera::Camera;
use crate::light::Light;
use crate::material::Material;
use crate::math::*;
use crate::scene::SceneBuilder;

/// A room lit by the sun and the sky through a glazed window, with a ceiling lamp.
pub fn interior(width: u32, height: u32) -> (SceneBuilder, Camera) {
    let mut b = SceneBuilder::new();
    let matte = |color: Vec3| Material { base_color: color, roughness: 0.9, ..Default::default() };

    let walls = b.add_material(matte(Vec3::new(0.82, 0.80, 0.76)));
    let floor = b.add_material(Material { base_color: Vec3::new(0.45, 0.30, 0.18), roughness: 0.35, ..Default::default() });
    let ceiling = b.add_material(matte(Vec3::splat(0.85)));
    let table = b.add_material(Material { base_color: Vec3::new(0.25, 0.25, 0.27), roughness: 0.3, clearcoat: 1.0, ..Default::default() });
    let steel = b.add_material(Material { base_color: Vec3::splat(0.9), metallic: 1.0, roughness: 0.25, ..Default::default() });
    let accent = b.add_material(matte(Vec3::new(0.65, 0.12, 0.10)));
    let glass = b.add_material(Material {
        base_color: Vec3::new(0.95, 0.98, 0.97),
        transmission: 1.0,
        thin: true,
        roughness: 0.0,
        ..Default::default()
    });
    let lamp = b.add_material(Material { base_color: Vec3::ZERO, emissive: Vec3::new(14.0, 11.0, 7.0), double_sided: false, ..Default::default() });

    // The room is 6m wide (x), 3m high (y) and 5m deep (z). The window is in the -x wall.
    let (x0, x1, y0, y1, z0, z1) = (-3.0, 3.0, 0.0, 3.0, -2.5, 2.5);
    let p = Vec3::new;
    b.add_quad([p(x0, y0, z1), p(x1, y0, z1), p(x1, y0, z0), p(x0, y0, z0)], floor);
    b.add_quad([p(x0, y1, z0), p(x1, y1, z0), p(x1, y1, z1), p(x0, y1, z1)], ceiling);
    b.add_quad([p(x0, y0, z0), p(x1, y0, z0), p(x1, y1, z0), p(x0, y1, z0)], walls); // back
    b.add_quad([p(x1, y0, z1), p(x0, y0, z1), p(x0, y1, z1), p(x1, y1, z1)], walls); // front
    b.add_quad([p(x1, y0, z0), p(x1, y0, z1), p(x1, y1, z1), p(x1, y1, z0)], walls); // right

    // left wall with a window opening from z = -1.5 to 1.5 and y = 0.8 to 2.4
    let (wz0, wz1, wy0, wy1) = (-1.5, 1.5, 0.8, 2.4);
    let left = |za: f32, zb: f32, ya: f32, yb: f32, b: &mut SceneBuilder| {
        b.add_quad([p(x0, ya, zb), p(x0, ya, za), p(x0, yb, za), p(x0, yb, zb)], walls);
    };
    left(z0, wz0, y0, y1, &mut b);
    left(wz1, z1, y0, y1, &mut b);
    left(wz0, wz1, y0, wy0, &mut b);
    left(wz0, wz1, wy1, y1, &mut b);
    b.add_quad([p(x0, wy0, wz1), p(x0, wy0, wz0), p(x0, wy1, wz0), p(x0, wy1, wz1)], glass);

    // furniture
    b.add_box(p(-0.9, 0.70, -0.5), p(0.9, 0.76, 0.5), table);
    for (x, z) in [(-0.8, -0.4), (0.74, -0.4), (-0.8, 0.34), (0.74, 0.34)] {
        b.add_box(p(x, 0.0, z), p(x + 0.06, 0.70, z + 0.06), steel);
    }
    b.add_box(p(-0.15, 0.76, -0.15), p(0.15, 1.06, 0.15), accent);
    b.add_box(p(1.6, 0.0, -2.3), p(2.6, 1.9, -1.9), steel);

    // ceiling lamp, facing down
    b.add_quad([p(0.4, 2.98, -0.3), p(1.0, 2.98, -0.3), p(1.0, 2.98, 0.3), p(0.4, 2.98, 0.3)], lamp);

    // afternoon sun coming through the window, plus a blue sky
    let to_sun = Vec3::new(-1.0, 0.75, 0.35).normalize();
    b.add_light(Light::Distant { dir: -to_sun, intensity: Vec3::new(20.0, 18.0, 15.0) });
    b.add_light(Light::Sky(crate::light::Sky { intensity: Vec3::new(1.2, 1.6, 2.2), env: None, rotation: 0.0 }));

    let camera = Camera::look_at(p(2.5, 1.5, 2.2), p(-1.2, 1.1, -0.4), Vec3::Y, 55f32.to_radians(), width, height);
    (b, camera)
}
