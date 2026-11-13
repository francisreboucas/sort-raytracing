// This file is a part of SORT-rs, a Rust port of SORT (Simple Open Ray Tracing).
// Original work Copyright (c) 2011-2023 by Jiayin Cao, GPLv3.

//! Cameras, ported from `src/camera/perspective.cpp`, `orthogonal.cpp` and `environment.cpp`.
//!
//! SORT has one class per camera; here they share the frame (eye and basis) and differ only in
//! how a raster position becomes a ray, so they are one struct with a projection.

use crate::math::*;
use std::f32::consts::PI;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Projection {
    /// Pinhole or thin lens camera, see `fov_y`, `lens_radius` and `focal_distance`.
    Perspective,
    /// Parallel rays, for elevations and plans. `height` is the height of the view volume in
    /// world units; its width follows from the image aspect ratio.
    Orthographic { height: f32 },
    /// Latitude-longitude panorama of the whole sphere around the eye, meant for a 2:1 image.
    Environment,
}

#[derive(Clone, Debug)]
pub struct Camera {
    pub eye: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub forward: Vec3,
    pub projection: Projection,
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
    /// Perspective camera.
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
            projection: Projection::Perspective,
            fov_y,
            lens_radius: 0.0,
            focal_distance: (target - eye).length(),
            width,
            height,
        }
    }

    /// Orthographic camera seeing a view volume `view_height` world units high.
    pub fn orthographic(eye: Vec3, target: Vec3, up: Vec3, view_height: f32, width: u32, height: u32) -> Self {
        let mut camera = Self::look_at(eye, target, up, 0.0, width, height);
        camera.projection = Projection::Orthographic { height: view_height };
        camera
    }

    /// Panorama camera, the center of the image looking at `target`.
    pub fn environment(eye: Vec3, target: Vec3, up: Vec3, width: u32, height: u32) -> Self {
        let mut camera = Self::look_at(eye, target, up, 0.0, width, height);
        camera.projection = Projection::Environment;
        camera
    }

    /// Generate a ray through raster position `(x, y)`, `dof` being the lens sample.
    pub fn generate_ray(&self, x: f32, y: f32, dof: (f32, f32)) -> Ray {
        let (w, h) = (self.width as f32, self.height as f32);
        match self.projection {
            Projection::Perspective => self.perspective_ray(x, y, dof),
            Projection::Orthographic { height } => {
                // the film is the view volume itself, every ray leaves it along `forward`
                let half = height * 0.5;
                let ori = self.eye
                    + self.right * ((2.0 * x / w - 1.0) * half * w / h)
                    + self.up * ((1.0 - 2.0 * y / h) * half);
                Ray::new(ori, self.forward, 0.0, f32::MAX)
            }
            Projection::Environment => {
                // azimuth from `forward` towards `right`, polar angle from `up`
                let phi = (x / w - 0.5) * 2.0 * PI;
                let theta = y / h * PI;
                let (sin_theta, cos_theta) = theta.sin_cos();
                let (sin_phi, cos_phi) = phi.sin_cos();
                let dir = self.up * cos_theta + (self.forward * cos_phi + self.right * sin_phi) * sin_theta;
                Ray::new(self.eye, dir.normalize(), 0.0, f32::MAX)
            }
        }
    }

    fn perspective_ray(&self, x: f32, y: f32, dof: (f32, f32)) -> Ray {
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

    #[test]
    fn orthographic_rays_are_parallel_and_span_the_view() {
        let mut cam = Camera::orthographic(Vec3::new(1.0, 2.0, 3.0), Vec3::new(4.0, 2.0, -1.0), Vec3::Y, 3.0, 400, 200);
        cam.lens_radius = 0.5; // ignored
        let ray = |x, y| cam.generate_ray(x, y, (0.3, 0.8));
        for (x, y) in [(0.0, 0.0), (400.0, 200.0), (123.0, 45.0)] {
            assert!((ray(x, y).dir - cam.forward).length() < 1e-6);
        }
        assert!((ray(200.0, 100.0).ori - cam.eye).length() < 1e-5);
        let vertical = ray(200.0, 0.0).ori - ray(200.0, 200.0).ori;
        assert!((vertical - cam.up * 3.0).length() < 1e-5);
        let horizontal = ray(400.0, 100.0).ori - ray(0.0, 100.0).ori;
        assert!((horizontal - cam.right * 6.0).length() < 1e-5);
    }

    #[test]
    fn environment_rays_cover_the_sphere() {
        let cam = Camera::environment(Vec3::new(1.0, 2.0, 3.0), Vec3::new(4.0, 2.0, -1.0), Vec3::Y, 400, 200);
        let dir = |x, y| cam.generate_ray(x, y, (0.5, 0.5)).dir;
        let close = |a: Vec3, b: Vec3| (a - b).length() < 1e-5;
        assert!(close(dir(200.0, 100.0), cam.forward));
        assert!(close(dir(200.0, 0.0), cam.up));
        assert!(close(dir(200.0, 200.0), -cam.up));
        assert!(close(dir(100.0, 100.0), -cam.right));
        assert!(close(dir(300.0, 100.0), cam.right));
        assert!(close(dir(0.0, 100.0), -cam.forward));
        for (x, y) in [(17.0, 33.0), (250.0, 180.0), (399.0, 1.0)] {
            assert!((dir(x, y).length() - 1.0).abs() < 1e-5);
        }
    }
}
