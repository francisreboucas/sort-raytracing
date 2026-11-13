// This file is a part of SORT-rs, a Rust port of SORT (Simple Open Ray Tracing).
// Original work Copyright (c) 2011-2023 by Jiayin Cao, GPLv3.

//! Perspective camera with depth of field, ported from `src/camera/perspective.cpp`.

use crate::math::*;

#[derive(Clone, Debug)]
pub struct Camera {
    pub eye: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub forward: Vec3,
    /// Vertical field of view in radians.
    pub fov_y: f32,
    /// Radius of the lens, zero for a pinhole camera.
    pub lens_radius: f32,
    /// Distance to the plane in perfect focus.
    pub focal_distance: f32,
    pub width: u32,
    pub height: u32,
}

impl Camera {
    pub fn look_at(eye: Vec3, target: Vec3, up: Vec3, fov_y: f32, width: u32, height: u32) -> Self {
        let forward = (target - eye).normalize();
        let mut right = forward.cross(up);
        if right.length_squared() < 1e-10 {
            right = coordinate_system(forward).0;
        }
        let right = right.normalize();
        Self {
            eye,
            right,
            up: right.cross(forward),
            forward,
            fov_y,
            lens_radius: 0.0,
            focal_distance: (target - eye).length(),
            width,
            height,
        }
    }

    /// Generate a ray through raster position `(x, y)`, `dof` being the lens sample.
    pub fn generate_ray(&self, x: f32, y: f32, dof: (f32, f32)) -> Ray {
        let (w, h) = (self.width as f32, self.height as f32);
        let tan_half_fov = (self.fov_y * 0.5).tan();
        let view_dir = Vec3::new(
            (2.0 * x / w - 1.0) * tan_half_fov * w / h,
            (1.0 - 2.0 * y / h) * tan_half_fov,
            1.0,
        );

        let mut ori = Vec3::ZERO;
        let mut dir = view_dir.normalize();

        // depth of field
        if self.lens_radius != 0.0 {
            let target = dir * (self.focal_distance / dir.z);
            let (s, t) = uniform_sample_disk(dof.0, dof.1);
            ori = Vec3::new(s * self.lens_radius, t * self.lens_radius, 0.0);
            dir = (target - ori).normalize();
        }

        // camera space to world space
        let to_world = |v: Vec3| self.right * v.x + self.up * v.y + self.forward * v.z;
        Ray::new(self.eye + to_world(ori), to_world(dir), 0.0, f32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn center_ray_looks_at_target() {
        let cam = Camera::look_at(Vec3::new(1.0, 2.0, 3.0), Vec3::new(4.0, 2.0, -1.0), Vec3::Y, 0.8, 640, 480);
        let ray = cam.generate_ray(320.0, 240.0, (0.5, 0.5));
        assert!((ray.dir - cam.forward).length() < 1e-5);
        // top of the image looks up, right of the image looks right
        assert!(cam.generate_ray(320.0, 0.0, (0.5, 0.5)).dir.dot(cam.up) > 0.0);
        assert!(cam.generate_ray(640.0, 240.0, (0.5, 0.5)).dir.dot(cam.right) > 0.0);
    }

    #[test]
    fn lens_rays_converge_on_focal_plane() {
        let mut cam = Camera::look_at(Vec3::ZERO, Vec3::new(0.0, 0.0, -5.0), Vec3::Y, 0.8, 200, 100);
        cam.lens_radius = 0.2;
        let a = cam.generate_ray(150.0, 30.0, (0.1, 0.2));
        let b = cam.generate_ray(150.0, 30.0, (0.9, 0.7));
        let on_plane = |r: &Ray| r.at(5.0 / r.dir.dot(cam.forward));
        assert!((a.ori - b.ori).length() > 1e-3);
        assert!((on_plane(&a) - on_plane(&b)).length() < 1e-4);
    }
}
