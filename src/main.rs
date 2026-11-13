// This file is a part of SORT-rs, a Rust port of SORT (Simple Open Ray Tracing).

use sort_rs::camera::Camera;
use sort_rs::light::{EnvMap, Light, Sky};
use sort_rs::math::*;
use sort_rs::render::{render, save_image, RenderSettings, ToneMap};
use sort_rs::{demo, gltf_loader};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

const USAGE: &str = "\
SORT-rs, a Rust port of the SORT renderer

USAGE:
    sort <scene.gltf|scene.glb> [options]
    sort --demo [options]

OPTIONS:
    -o, --output <file>       Output image, .png/.jpg (tone mapped) or .exr/.hdr (linear) [render.png]
    -w, --width <pixels>      Image width [1280]
    -h, --height <pixels>     Image height [from the camera aspect ratio, else 720]
    -s, --spp <count>         Samples per pixel [64]
    -d, --depth <count>       Maximum path depth [6]
        --camera <index>      Which camera of the file to use [0]
        --fov <degrees>       Override the vertical field of view
        --aperture <radius>   Lens radius for depth of field [0]
        --focus <distance>    Focus distance for depth of field
        --sky <r,g,b>         Constant sky radiance
        --env <file.hdr>      HDR environment map (latitude-longitude)
        --env-intensity <f>   Environment map multiplier [1]
        --env-rotate <deg>    Rotate the environment map around the vertical axis [0]
        --sun <az,el>         Add a sun, azimuth and elevation in degrees
        --sun-intensity <f>   Irradiance of the sun [20]
        --exposure <f>        Exposure multiplier [1]
        --tonemap <aces|none> Tone mapping for .png/.jpg output [aces]
        --filter-glossy <f>   Blur of polished surfaces seen after a rough bounce, which turns
                              caustic fireflies into soft light; 0 renders caustics exactly [0.5]
        --clamp <f>           Clamp sample radiance to reduce fireflies (biased)
        --seed <n>            Random seed [0]
        --threads <n>         Number of threads [all cores]
        --help                Print this message
";

struct Args {
    scene: Option<PathBuf>,
    demo: bool,
    output: PathBuf,
    width: u32,
    height: Option<u32>,
    camera: usize,
    fov: Option<f32>,
    aperture: f32,
    focus: Option<f32>,
    sky: Option<Vec3>,
    env: Option<PathBuf>,
    env_intensity: f32,
    env_rotate: f32,
    sun: Option<(f32, f32)>,
    sun_intensity: f32,
    exposure: f32,
    tone_map: ToneMap,
    threads: Option<usize>,
    settings: RenderSettings,
}

fn parse_list(value: &str, count: usize) -> Result<Vec<f32>, String> {
    let list: Result<Vec<f32>, _> = value.split(',').map(|v| v.trim().parse::<f32>()).collect();
    match list {
        Ok(list) if list.len() == count => Ok(list),
        _ => Err(format!("expected {count} comma separated numbers, got '{value}'")),
    }
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args {
        scene: None,
        demo: false,
        output: PathBuf::from("render.png"),
        width: 1280,
        height: None,
        camera: 0,
        fov: None,
        aperture: 0.0,
        focus: None,
        sky: None,
        env: None,
        env_intensity: 1.0,
        env_rotate: 0.0,
        sun: None,
        sun_intensity: 20.0,
        exposure: 1.0,
        tone_map: ToneMap::Aces,
        threads: None,
        settings: RenderSettings::default(),
    };

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("missing value for {arg}"));
        fn num<T: std::str::FromStr>(name: &str, v: String) -> Result<T, String> {
            v.parse().map_err(|_| format!("invalid value '{v}' for {name}"))
        }
        match arg.as_str() {
            "--help" => return Ok(None),
            "--demo" => args.demo = true,
            "-o" | "--output" => args.output = PathBuf::from(value()?),
            "-w" | "--width" => args.width = num(&arg, value()?)?,
            "-h" | "--height" => args.height = Some(num(&arg, value()?)?),
            "-s" | "--spp" => args.settings.samples_per_pixel = num(&arg, value()?)?,
            "-d" | "--depth" => args.settings.max_depth = num(&arg, value()?)?,
            "--camera" => args.camera = num(&arg, value()?)?,
            "--fov" => args.fov = Some(num(&arg, value()?)?),
            "--aperture" => args.aperture = num(&arg, value()?)?,
            "--focus" => args.focus = Some(num(&arg, value()?)?),
            "--sky" => {
                let c = parse_list(&value()?, 3)?;
                args.sky = Some(Vec3::new(c[0], c[1], c[2]));
            }
            "--env" => args.env = Some(PathBuf::from(value()?)),
            "--env-intensity" => args.env_intensity = num(&arg, value()?)?,
            "--env-rotate" => args.env_rotate = num(&arg, value()?)?,
            "--sun" => {
                let c = parse_list(&value()?, 2)?;
                args.sun = Some((c[0], c[1]));
            }
            "--sun-intensity" => args.sun_intensity = num(&arg, value()?)?,
            "--exposure" => args.exposure = num(&arg, value()?)?,
            "--tonemap" => {
                args.tone_map = match value()?.as_str() {
                    "aces" => ToneMap::Aces,
                    "none" => ToneMap::None,
                    other => return Err(format!("unknown tone map '{other}'")),
                }
            }
            "--clamp" => args.settings.clamp = Some(num(&arg, value()?)?),
            "--filter-glossy" => args.settings.filter_glossy = num(&arg, value()?)?,
            "--seed" => args.settings.seed = num(&arg, value()?)?,
            "--threads" => args.threads = Some(num(&arg, value()?)?),
            other if other.starts_with('-') => return Err(format!("unknown option '{other}'")),
            _ => args.scene = Some(PathBuf::from(arg)),
        }
    }

    if args.scene.is_none() && !args.demo {
        return Err("no scene given".into());
    }
    if args.width == 0 || args.height == Some(0) {
        return Err("the image size must not be zero".into());
    }
    Ok(Some(args))
}

fn run(args: Args) -> Result<(), String> {
    if let Some(threads) = args.threads {
        rayon::ThreadPoolBuilder::new().num_threads(threads).build_global().map_err(|e| e.to_string())?;
    }

    let start = Instant::now();
    let (mut builder, camera) = match &args.scene {
        Some(path) => {
            let loaded = gltf_loader::load(path).map_err(|e| format!("failed to load {}: {e}", path.display()))?;
            let desc = loaded.cameras.get(args.camera);
            if desc.is_none() && args.camera != 0 {
                return Err(format!("the file has {} camera(s), index {} is out of range", loaded.cameras.len(), args.camera));
            }
            let aspect = desc.and_then(|d| d.aspect_ratio).unwrap_or(16.0 / 9.0);
            let height = args.height.unwrap_or(((args.width as f32 / aspect).round() as u32).max(1));
            // without a camera in the file, the scene is framed once its bounds are known
            let camera = desc.map(|d| d.to_camera(args.width, height));
            (loaded.builder, camera.ok_or(height))
        }
        None => {
            let height = args.height.unwrap_or((args.width * 9 / 16).max(1));
            let (builder, camera) = demo::interior(args.width, height);
            (builder, Ok(camera))
        }
    };

    // lights from the command line
    if args.sky.is_some() || args.env.is_some() {
        builder.lights.retain(|l| !matches!(l, Light::Sky(_)));
        let env = match &args.env {
            Some(path) => {
                let img = image::open(path).map_err(|e| format!("failed to load {}: {e}", path.display()))?.into_rgb32f();
                let (w, h) = (img.width() as usize, img.height() as usize);
                let pixels = img.pixels().map(|p| Vec3::new(p[0], p[1], p[2])).collect();
                Some(EnvMap::new(w, h, pixels))
            }
            None => None,
        };
        let intensity = match (&env, args.sky) {
            (Some(_), _) => Vec3::splat(args.env_intensity),
            (None, Some(color)) => color,
            (None, None) => Vec3::ONE,
        };
        builder.add_light(Light::Sky(Sky { intensity, env, rotation: args.env_rotate.to_radians() }));
    }
    if let Some((azimuth, elevation)) = args.sun {
        let (az, el) = (azimuth.to_radians(), elevation.to_radians());
        let to_sun = Vec3::new(el.cos() * az.cos(), el.sin(), el.cos() * az.sin());
        builder.add_light(Light::Distant { dir: -to_sun, intensity: Vec3::splat(args.sun_intensity) });
    }
    let has_emitters = builder.materials.iter().any(|m| m.is_emissive());
    if builder.lights.is_empty() && !has_emitters {
        eprintln!("note: the scene has no light, adding a uniform white sky (see --sky, --env and --sun)");
        builder.add_light(Light::Sky(Sky { intensity: Vec3::ONE, env: None, rotation: 0.0 }));
    }

    let scene = builder.build();
    if scene.triangle_count() == 0 {
        return Err("the scene has no triangles".into());
    }
    let (nodes, depth) = scene.bvh_stats();
    eprintln!(
        "scene: {} triangles, {} materials, {} lights, bvh with {nodes} nodes and depth {depth}, loaded in {:.2?}",
        scene.triangle_count(),
        scene.materials.len(),
        scene.lights.len(),
        start.elapsed()
    );

    let mut camera = match camera {
        Ok(camera) => camera,
        Err(height) => {
            eprintln!("note: the file has no perspective camera, framing the whole scene");
            let center = scene.bbox.center();
            let radius = (scene.bbox.max - scene.bbox.min).length() * 0.5;
            let eye = center + Vec3::new(1.0, 0.6, 1.0).normalize() * radius * 2.2;
            Camera::look_at(eye, center, Vec3::Y, 40f32.to_radians(), args.width, height)
        }
    };
    if let Some(fov) = args.fov {
        camera.fov_y = fov.to_radians();
    }
    camera.lens_radius = args.aperture;
    if let Some(focus) = args.focus {
        camera.focal_distance = focus;
    }

    eprintln!(
        "rendering {}x{} at {} spp, depth {}",
        camera.width, camera.height, args.settings.samples_per_pixel, args.settings.max_depth
    );
    let start = Instant::now();
    let last_percent = AtomicU32::new(0);
    let pixels = render(&scene, &camera, &args.settings, |fraction| {
        let percent = (fraction * 100.0) as u32;
        if last_percent.fetch_max(percent, Ordering::Relaxed) < percent && percent % 5 == 0 {
            eprintln!("  {percent:3}%  ({:.1?})", start.elapsed());
        }
    });
    eprintln!("rendered in {:.2?}", start.elapsed());

    save_image(&args.output, &pixels, camera.width, camera.height, args.exposure, args.tone_map)
        .map_err(|e| format!("failed to write {}: {e}", args.output.display()))?;
    eprintln!("wrote {}", args.output.display());
    Ok(())
}

fn main() -> ExitCode {
    match parse_args() {
        Ok(Some(args)) => match run(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Ok(None) => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}
