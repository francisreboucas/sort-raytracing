// This file is a part of SORT-rs, a Rust port of SORT (Simple Open Ray Tracing).

use sort_raytracing::camera::{Camera, Projection};
use sort_raytracing::light::{EnvMap, Light, Sky};
use sort_raytracing::math::*;
use sort_raytracing::medium::{Fog, Medium};
use sort_raytracing::render::{render, save_image, Integrator, Progressive, Region, RenderSettings, ToneMap};
use sort_raytracing::{demo, gltf_loader};
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
    -h, --height <pixels>     Image height [from the camera aspect ratio, else 720; width/2 for a panorama]
    -s, --spp <count>         Samples per pixel [64]
    -d, --depth <count>       Maximum path depth [6]
        --integrator <i>      pt (path tracing) or ao (ambient occlusion: a white \"clay\" render
                              without materials or lights) [pt]
        --ao-distance <m>     Distance within which geometry darkens a point (ao) [3]
        --region <x,y,w,h>    Render this rectangle of the image only, in pixels from the top
                              left corner; the rest of the image is left black
        --camera <index>      Which camera of the file to use [0]
        --projection <p>      perspective, ortho or panorama (360 degrees, latitude-longitude)
                              [the projection of the camera]
        --ortho-height <m>    Height of the orthographic view in meters [the camera's if orthographic,
                              else the whole scene when framed, else what the perspective camera
                              sees at the distance of the scene's center]
        --eye <x,y,z>         Place the camera here, in meters (y is up)
        --target <x,y,z>      Point the camera at this position [the center of the scene with --eye]
        --fov <degrees>       Override the vertical field of view (perspective)
        --aperture <radius>   Lens radius for depth of field [0] (perspective)
        --focus <distance>    Focus distance for depth of field (perspective)
        --sky <r,g,b>         Constant sky radiance
        --env <file.hdr>      HDR environment map (latitude-longitude)
        --env-intensity <f>   Environment map multiplier [1]
        --env-rotate <deg>    Rotate the environment map around the vertical axis [0]
        --sun <az,el>         Add a sun, azimuth and elevation in degrees
        --sun-intensity <f>   Irradiance of the sun [20]
        --fog <density>       Fill the scene with fog or haze that removes this fraction of the
                              light per meter: 0.01 is a light haze, 0.2 a dusty room
        --fog-color <r,g,b>   Part of that light the fog scatters, the rest is absorbed [1,1,1]
        --fog-anisotropy <g>  -1 to 1: 0 scatters evenly, towards 1 forward (haze is near 0.8) [0]
        --fog-height <m>      Make the fog a layer below this height that goes on forever
                              sideways, for exteriors [a box around the scene]
        --exposure <f>        Exposure multiplier [1]
        --tonemap <aces|none> Tone mapping for .png/.jpg output [aces, none with ao]
        --filter-glossy <f>   Blur of polished surfaces seen after a rough bounce, which turns
                              caustic fireflies into soft light; 0 renders caustics exactly [0.5]
        --clamp <f>           Clamp sample radiance to reduce fireflies (biased)
        --seed <n>            Random seed [0]
        --threads <n>         Number of threads [all cores]
        --help                Print this message
";

#[derive(Clone, Copy, PartialEq)]
enum ProjectionArg {
    Perspective,
    Ortho,
    Panorama,
}

struct Args {
    scene: Option<PathBuf>,
    demo: bool,
    output: PathBuf,
    width: u32,
    height: Option<u32>,
    region: Option<Region>,
    camera: usize,
    projection: Option<ProjectionArg>,
    ortho_height: Option<f32>,
    fov: Option<f32>,
    aperture: f32,
    focus: Option<f32>,
    sky: Option<Vec3>,
    env: Option<PathBuf>,
    env_intensity: f32,
    env_rotate: f32,
    sun: Option<(f32, f32)>,
    fog: Option<f32>,
    fog_color: Vec3,
    fog_anisotropy: f32,
    fog_height: Option<f32>,
    eye: Option<Vec3>,
    target: Option<Vec3>,
    sun_intensity: f32,
    exposure: f32,
    /// `None` when not given: it then depends on the integrator.
    tone_map: Option<ToneMap>,
    ao: bool,
    ao_distance: f32,
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
        region: None,
        camera: 0,
        projection: None,
        ortho_height: None,
        fov: None,
        aperture: 0.0,
        focus: None,
        sky: None,
        env: None,
        env_intensity: 1.0,
        env_rotate: 0.0,
        sun: None,
        fog: None,
        fog_color: Vec3::ONE,
        fog_anisotropy: 0.0,
        fog_height: None,
        eye: None,
        target: None,
        sun_intensity: 20.0,
        exposure: 1.0,
        tone_map: None,
        ao: false,
        ao_distance: 3.0,
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
            "--region" => {
                let value = value()?;
                let list: Result<Vec<u32>, _> = value.split(',').map(|v| v.trim().parse::<u32>()).collect();
                args.region = Some(match list.as_deref() {
                    Ok(&[x, y, width, height]) => Region { x, y, width, height },
                    _ => return Err(format!("expected 4 comma separated whole numbers, got '{value}'")),
                });
            }
            "--camera" => args.camera = num(&arg, value()?)?,
            "--projection" => {
                args.projection = Some(match value()?.as_str() {
                    "perspective" => ProjectionArg::Perspective,
                    "ortho" | "orthographic" => ProjectionArg::Ortho,
                    "panorama" | "environment" => ProjectionArg::Panorama,
                    other => return Err(format!("unknown projection '{other}'")),
                })
            }
            "--ortho-height" => args.ortho_height = Some(num(&arg, value()?)?),
            "--eye" => {
                let c = parse_list(&value()?, 3)?;
                args.eye = Some(Vec3::new(c[0], c[1], c[2]));
            }
            "--target" => {
                let c = parse_list(&value()?, 3)?;
                args.target = Some(Vec3::new(c[0], c[1], c[2]));
            }
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
            "--fog" => args.fog = Some(num(&arg, value()?)?),
            "--fog-color" => {
                let c = parse_list(&value()?, 3)?;
                args.fog_color = Vec3::new(c[0], c[1], c[2]);
            }
            "--fog-anisotropy" => args.fog_anisotropy = num(&arg, value()?)?,
            "--fog-height" => args.fog_height = Some(num(&arg, value()?)?),
            "--exposure" => args.exposure = num(&arg, value()?)?,
            "--tonemap" => {
                args.tone_map = Some(match value()?.as_str() {
                    "aces" => ToneMap::Aces,
                    "none" => ToneMap::None,
                    other => return Err(format!("unknown tone map '{other}'")),
                })
            }
            "--integrator" => {
                args.ao = match value()?.as_str() {
                    "pt" | "path" => false,
                    "ao" | "occlusion" => true,
                    other => return Err(format!("unknown integrator '{other}'")),
                }
            }
            "--ao-distance" => args.ao_distance = num(&arg, value()?)?,
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
    if args.region.is_some_and(|r| r.width == 0 || r.height == 0) {
        return Err("the region size must not be zero".into());
    }
    if args.ortho_height.is_some_and(|h| !(h > 0.0)) {
        return Err("the orthographic height must be positive".into());
    }
    if args.fog.is_some_and(|d| !(d >= 0.0)) {
        return Err("the fog density must not be negative".into());
    }
    if !(args.fog_anisotropy > -1.0 && args.fog_anisotropy < 1.0) {
        return Err("the fog anisotropy must be between -1 and 1".into());
    }
    if !(args.ao_distance > 0.0) {
        return Err("the ambient occlusion distance must be positive".into());
    }
    if args.ao {
        args.settings.integrator = Integrator::AmbientOcclusion { max_distance: args.ao_distance };
    }
    Ok(Some(args))
}

fn run(args: Args) -> Result<(), String> {
    if let Some(threads) = args.threads {
        rayon::ThreadPoolBuilder::new().num_threads(threads).build_global().map_err(|e| e.to_string())?;
    }

    let start = Instant::now();
    let panorama = args.projection == Some(ProjectionArg::Panorama);
    let (mut builder, camera) = match &args.scene {
        Some(path) => {
            let loaded = gltf_loader::load(path).map_err(|e| format!("failed to load {}: {e}", path.display()))?;
            let desc = loaded.cameras.get(args.camera);
            if desc.is_none() && args.camera != 0 {
                return Err(format!("the file has {} camera(s), index {} is out of range", loaded.cameras.len(), args.camera));
            }
            // a latitude-longitude panorama is 2:1 whatever the camera
            let aspect = if panorama { 2.0 } else { desc.and_then(|d| d.aspect_ratio).unwrap_or(16.0 / 9.0) };
            let height = args.height.unwrap_or(((args.width as f32 / aspect).round() as u32).max(1));
            // without a camera in the file, the scene is framed once its bounds are known
            let camera = desc.map(|d| d.to_camera(args.width, height));
            (loaded.builder, camera.ok_or(height))
        }
        None => {
            let default_height = if panorama { args.width / 2 } else { args.width * 9 / 16 };
            let height = args.height.unwrap_or(default_height.max(1));
            let (builder, camera) = demo::interior(args.width, height);
            (builder, Ok(camera))
        }
    };

    // lights from the command line
    if args.sky.is_some() || args.env.is_some() {
        builder.lights.retain(|l| !matches!(l, Light::Sky(_)));
        let env = match &args.env {
            Some(path) => {
                let failed = |e: &dyn std::fmt::Display| format!("failed to load {}: {e}", path.display());
                // large sky maps go over the default memory limit of the decoder
                let mut reader = image::ImageReader::open(path).map_err(|e| failed(&e))?;
                reader.no_limits();
                let img = reader.with_guessed_format().map_err(|e| failed(&e))?.decode().map_err(|e| failed(&e))?;
                let img = img.into_rgb32f();
                let (w, h) = (img.width() as usize, img.height() as usize);
                // Maps wider than MAX_ENV_WIDTH are averaged down by a whole factor, which keeps
                // the energy of the sun and bounds the memory of the map and its sampling tables.
                const MAX_ENV_WIDTH: usize = 8192;
                let factor = w.div_ceil(MAX_ENV_WIDTH).max(1);
                let (sw, sh) = ((w / factor).max(1), (h / factor).max(1));
                let texel = |x: usize, y: usize| {
                    let p = img.get_pixel(x.min(w - 1) as u32, y.min(h - 1) as u32);
                    Vec3::new(p[0], p[1], p[2])
                };
                let pixels = (0..sw * sh)
                    .map(|i| {
                        let (x, y) = (i % sw * factor, i / sw * factor);
                        let sum: Vec3 = (0..factor * factor).map(|k| texel(x + k % factor, y + k / factor)).sum();
                        sum / (factor * factor) as f32
                    })
                    .collect();
                if factor > 1 {
                    eprintln!("environment map reduced from {w}x{h} to {sw}x{sh}");
                }
                Some(EnvMap::new(sw, sh, pixels))
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
    if let Some(density) = args.fog {
        let bounds = builder.bounds();
        let (min, max) = match args.fog_height {
            Some(height) => (Vec3::NEG_INFINITY, Vec3::new(f32::INFINITY, height, f32::INFINITY)),
            None => (bounds.min, bounds.max),
        };
        let medium = Medium::scattering(density, args.fog_color, args.fog_anisotropy);
        builder.fog = Some(Fog { medium, min, max });
    }
    let has_emitters = builder.materials.iter().any(|m| m.is_emissive());
    if builder.lights.is_empty() && !has_emitters {
        // ambient occlusion ignores lights, the sky is harmless there
        if !args.ao {
            eprintln!("note: the scene has no light, adding a uniform white sky (see --sky, --env and --sun)");
        }
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
    eprintln!("bounds: {:.2} to {:.2}", scene.bbox.min, scene.bbox.max);

    // radius of the bounding sphere when the camera frames the whole scene
    let mut framed_radius = None;
    let mut camera = match camera {
        Ok(camera) => camera,
        Err(height) => {
            eprintln!("note: the file has no camera, framing the whole scene");
            let center = scene.bbox.center();
            let radius = (scene.bbox.max - scene.bbox.min).length() * 0.5;
            let eye = center + Vec3::new(1.0, 0.6, 1.0).normalize() * radius * 2.2;
            framed_radius = Some(radius);
            Camera::look_at(eye, center, Vec3::Y, 40f32.to_radians(), args.width, height)
        }
    };
    if args.eye.is_some() || args.target.is_some() {
        let eye = args.eye.unwrap_or(camera.eye);
        let target = args.target.unwrap_or(scene.bbox.center());
        if (target - eye).length_squared() < 1e-12 {
            return Err("--eye and --target must be different positions".into());
        }
        let projection = camera.projection;
        camera = Camera::look_at(eye, target, Vec3::Y, camera.fov_y, camera.width, camera.height);
        camera.projection = projection;
        framed_radius = None;
    }
    if let Some(fov) = args.fov {
        camera.fov_y = fov.to_radians();
    }
    camera.lens_radius = args.aperture;
    if let Some(focus) = args.focus {
        camera.focal_distance = focus;
    }
    camera.projection = match (args.projection, camera.projection) {
        (Some(ProjectionArg::Perspective), _) => Projection::Perspective,
        (Some(ProjectionArg::Panorama), _) => Projection::Environment,
        (Some(ProjectionArg::Ortho), current) | (None, current @ Projection::Orthographic { .. }) => {
            let height = match (args.ortho_height, current, framed_radius) {
                (Some(height), _, _) => height,
                (None, Projection::Orthographic { height }, _) => height,
                // the bounding sphere fits both vertically and horizontally
                (None, _, Some(radius)) => 2.0 * radius * (camera.height as f32 / camera.width as f32).max(1.0),
                // What the perspective camera sees at the distance of the scene's center. Its
                // focal distance is no guide: glTF cameras have none and get 1.
                (None, _, None) => {
                    let along = (scene.bbox.center() - camera.eye).dot(camera.forward);
                    let distance = match args.focus {
                        Some(focus) => focus,
                        None if along > 0.0 => along,
                        None => camera.focal_distance,
                    };
                    2.0 * distance * (camera.fov_y * 0.5).tan()
                }
            };
            Projection::Orthographic { height }
        }
        (None, current) => current,
    };

    let view = match camera.projection {
        Projection::Perspective => "perspective".to_string(),
        Projection::Orthographic { height } => format!("orthographic ({height:.2} m high)"),
        Projection::Environment => "panorama".to_string(),
    };
    match args.settings.integrator {
        Integrator::PathTracing => eprintln!(
            "rendering {}x{} {view} at {} spp, depth {}",
            camera.width, camera.height, args.settings.samples_per_pixel, args.settings.max_depth
        ),
        Integrator::AmbientOcclusion { max_distance } => eprintln!(
            "rendering {}x{} {view} at {} spp, ambient occlusion within {max_distance} m",
            camera.width, camera.height, args.settings.samples_per_pixel
        ),
    }
    let region = match args.region {
        Some(region) if region.x >= camera.width || region.y >= camera.height => {
            return Err(format!(
                "the region starts at {},{}, outside of the {}x{} image",
                region.x, region.y, camera.width, camera.height
            ));
        }
        region => region,
    };
    let start = Instant::now();
    let pixels = match region {
        Some(region) => {
            let mut render = Progressive::with_region(&scene, &camera, &args.settings, region);
            let region = render.region();
            eprintln!("region of {}x{} at {},{}", region.width, region.height, region.x, region.y);
            // one sample per pixel at a time: the progress is the samples done
            let mut last_percent = 0;
            while render.step() {
                let percent = render.samples() * 100 / render.total_samples();
                if percent / 5 > last_percent / 5 {
                    eprintln!("  {:3}%  ({:.1?})", percent / 5 * 5, start.elapsed());
                }
                last_percent = percent;
            }
            render.image()
        }
        None => {
            let last_percent = AtomicU32::new(0);
            render(&scene, &camera, &args.settings, |fraction| {
                let percent = (fraction * 100.0) as u32;
                if last_percent.fetch_max(percent, Ordering::Relaxed) < percent && percent % 5 == 0 {
                    eprintln!("  {percent:3}%  ({:.1?})", start.elapsed());
                }
            })
        }
    };
    eprintln!("rendered in {:.2?}", start.elapsed());

    // ambient occlusion is display-referred already, ACES would grey its white
    let tone_map = args.tone_map.unwrap_or(if args.ao { ToneMap::None } else { ToneMap::Aces });
    save_image(&args.output, &pixels, camera.width, camera.height, args.exposure, tone_map)
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
