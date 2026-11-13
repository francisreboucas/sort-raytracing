//! Compare a render against a reference image: `compare <reference> <image>`.
//! Prints the mean luminance of both and the relative mean squared error.

use sort_raytracing::math::intensity;
use sort_raytracing::math::Vec3;

fn load(path: &str) -> (u32, u32, Vec<Vec3>) {
    let img = image::open(path).unwrap_or_else(|e| panic!("failed to open {path}: {e}")).into_rgb32f();
    (img.width(), img.height(), img.pixels().map(|p| Vec3::new(p[0], p[1], p[2])).collect())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 && args.len() != 4 {
        eprintln!("usage: compare <reference> <image> [error-map.png]");
        std::process::exit(2);
    }
    let (rw, rh, reference) = load(&args[1]);
    let (w, h, image) = load(&args[2]);
    assert_eq!((rw, rh), (w, h), "the images must have the same size");

    let n = reference.len() as f64;
    let mean = |px: &[Vec3]| px.iter().map(|p| intensity(*p) as f64).sum::<f64>() / n;
    let rel_mse = reference
        .iter()
        .zip(&image)
        .map(|(r, i)| {
            let d = *i - *r;
            (d.dot(d) / (r.dot(*r) + 1e-2)) as f64 / 3.0
        })
        .sum::<f64>()
        / n;
    if let Some(path) = args.get(3) {
        // relative error of each pixel, white at 100%
        let data: Vec<u8> = reference
            .iter()
            .zip(&image)
            .map(|(r, i)| {
                let d = *i - *r;
                let e = (d.dot(d) / (r.dot(*r) + 1e-2)).sqrt();
                (e.min(1.0) * 255.0) as u8
            })
            .collect();
        image::GrayImage::from_raw(w, h, data).unwrap().save(path).unwrap();
    }
    // how much of the error sits in the worst 1% of the pixels: fireflies or plain noise?
    let mut errors: Vec<f64> = reference
        .iter()
        .zip(&image)
        .map(|(r, i)| {
            let d = *i - *r;
            (d.dot(d) / (r.dot(*r) + 1e-2)) as f64
        })
        .collect();
    errors.sort_by(|a, b| b.partial_cmp(a).unwrap());
    let total: f64 = errors.iter().sum();
    let top: f64 = errors[..errors.len() / 100].iter().sum();
    let median = errors[errors.len() / 2] / 3.0;
    println!("worst 1% of pixels hold {:.0}% of the error, median pixel relMSE {:.6}", 100.0 * top / total, median);
    println!("mean {:.5} vs {:.5}   relMSE {:.6}", mean(&reference), mean(&image), rel_mse);
}
