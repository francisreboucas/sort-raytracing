//! Noise diagnostics on the built-in interior: per-pixel variance of the path tracer with one
//! kind of light at a time. `variance [depth] [filter-glossy]`

use rayon::prelude::*;
use sort_rs::demo;
use sort_rs::integrator::PathTracing;
use sort_rs::light::Light;
use sort_rs::math::*;
use sort_rs::rng::Rng;

fn main() {
    let depth: u32 = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(1);
    let filter_glossy: f32 = std::env::args().nth(2).and_then(|a| a.parse().ok()).unwrap_or(1.0);
    let (width, height, samples) = (160u32, 90u32, 256u32);

    for config in ["all", "sun", "sky", "lamp"] {
        let (mut builder, camera) = demo::interior(width, height);
        if config != "all" {
            builder.lights.retain(|l| match l {
                Light::Distant { .. } => config == "sun",
                Light::Sky(_) => config == "sky",
                _ => false,
            });
            if config != "lamp" {
                builder.materials.iter_mut().for_each(|m| m.emissive = Vec3::ZERO);
            }
        }
        let scene = builder.build();
        let integrator = PathTracing { max_recursive_depth: depth, filter_glossy };

        // relative variance of a single sample, for every pixel
        let mut stats: Vec<(f64, f64, u32, u32)> = (0..width * height)
            .into_par_iter()
            .map(|i| {
                let (x, y) = (i % width, i / width);
                let mut rng = Rng::new(7, i as u64);
                let (mut sum, mut sum2) = (0.0f64, 0.0f64);
                for _ in 0..samples {
                    let (jx, jy) = rng.pair();
                    let ray = camera.generate_ray(x as f32 + jx, y as f32 + jy, rng.pair());
                    let v = intensity(integrator.li(&ray, &scene, &mut rng)) as f64;
                    sum += v;
                    sum2 += v * v;
                }
                let mean = sum / samples as f64;
                let variance = (sum2 / samples as f64 - mean * mean).max(0.0);
                (variance / (mean * mean + 1e-4), mean, x, y)
            })
            .collect();

        let mean_image = stats.iter().map(|s| s.1).sum::<f64>() / stats.len() as f64;
        stats.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let at = |q: f64| stats[((stats.len() - 1) as f64 * q) as usize].0;
        let worst = stats.last().unwrap();
        println!(
            "{config:5} mean {mean_image:.4}  relative variance per sample: median {:.3}  90% {:.3}  99% {:.3}  max {:.1} at ({}, {})",
            at(0.5),
            at(0.9),
            at(0.99),
            worst.0,
            worst.2,
            worst.3
        );
    }
}
