//! Showing a render while it converges, and rendering a region of the image only: what an
//! application with a viewport does. Here the "viewport" is a file, written again as the
//! image gets cleaner.
//!
//! `cargo run --release --example progressive`

use sort_raytracing::demo;
use sort_raytracing::render::{save_image, Progressive, Region, RenderSettings, ToneMap};
use std::path::Path;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (builder, camera) = demo::interior(640, 360);
    let scene = builder.build();
    let settings = RenderSettings { samples_per_pixel: 64, ..Default::default() };
    let (width, height) = (camera.width, camera.height);

    // The whole image, one sample per pixel at a time. An application would show the image
    // after every step, and leave the loop when the camera moves or the view is closed.
    let start = Instant::now();
    let mut render = Progressive::new(&scene, &camera, &settings);
    while render.step() {
        let samples = render.samples();
        // 1, 2, 4, 8... samples: writing a file at every step would take longer than rendering
        if samples.is_power_of_two() || render.is_done() {
            save_image(Path::new("progressive.png"), &render.image(), width, height, 1.0, ToneMap::Aces)?;
            println!("{samples:3}/{} samples  ({:.1?})", render.total_samples(), start.elapsed());
        }
    }

    // A region only, as when the user draws a rectangle on the view: the rest is not traced,
    // so the same time buys many more samples there.
    let region = Region { x: 220, y: 120, width: 200, height: 120 };
    let start = Instant::now();
    let mut render = Progressive::with_region(&scene, &camera, &settings, region);
    // several samples per step when nobody watches every one of them
    while render.step_by(16) > 0 {}
    save_image(Path::new("progressive-region.png"), &render.image(), width, height, 1.0, ToneMap::Aces)?;
    // or the region alone, as an image of its own size
    save_image(Path::new("progressive-crop.png"), &render.region_pixels(), region.width, region.height, 1.0, ToneMap::Aces)?;
    println!("region of {}x{} at {} samples in {:.1?}", region.width, region.height, render.samples(), start.elapsed());
    Ok(())
}
