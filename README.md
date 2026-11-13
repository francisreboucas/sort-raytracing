# SORT-rs

[![License: GPL-3.0-only](https://img.shields.io/badge/license-GPL--3.0--only-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-pure_Rust-orange.svg?logo=rust)](https://www.rust-lang.org/)

A path tracer for architectural rendering, in pure Rust. It is a port of
[SORT](https://github.com/JiayinCao/SORT) (Simple Open-source Ray Tracing, by Jiayin Cao), with
the lighting reworked for interiors.

[Português (Brasil)](README.pt-BR.md)

![A room lit by the sun and the sky through a glazed window](assets/readme/interior.png)

The built-in interior (`sort --demo`), 960x540 at 320 samples per pixel: sun and sky through a
glazed window, a ceiling lamp, a coated table top and steel legs.

## Status

Early. The renderer is tested against analytic results and on its built-in scenes; it has not
been run on a large architectural model yet. See [what is not ported](#not-ported-yet).

## Running it

```sh
cargo build --release

cargo run --release -- --demo -o demo.png                 # the built-in interior
cargo run --release -- house.glb -o house.png -w 1920 --spp 512 --depth 8
cargo run --release -- house.glb --env sky.hdr --env-rotate 90      # lit by an HDR environment
cargo run --release -- house.glb --sun 135,40 --sky 1.2,1.6,2.2     # a sun (azimuth, elevation) and a plain sky
cargo run --release -- house.glb -o house.exr             # linear radiance
cargo run --release -- --help
```

Scenes are glTF 2.0 (`.gltf` or `.glb`): meshes, metallic-roughness materials with their
textures, cameras and the lights of `KHR_lights_punctual`. Lengths are in meters and Y is up.
`.png` and `.jpg` images are tone mapped (ACES by default) and sRGB encoded; `.exr` and `.hdr`
hold linear radiance.

## As a library

The scene can be built in code, with no file involved
([`examples/library.rs`](examples/library.rs)):

```rust
let mut builder = SceneBuilder::new();
let plaster = builder.add_material(Material { base_color: Vec3::new(0.8, 0.78, 0.74), roughness: 0.9, ..Default::default() });
builder.add_quad([/* four corners, counter-clockwise */], plaster);
// ... the rest of the room, with a doorway in one wall ...

// where daylight comes in
builder.add_portal([/* the four corners of the doorway */]);

builder.add_light(Light::Distant { dir: -to_sun, intensity: Vec3::new(20.0, 18.0, 15.0) });
builder.add_light(Light::Sky(Sky { intensity: Vec3::new(1.2, 1.6, 2.2), env: None, rotation: 0.0 }));

let scene = builder.build();
let camera = Camera::look_at(eye, target, Vec3::Y, 60f32.to_radians(), 640, 360);
let pixels = render(&scene, &camera, &RenderSettings::default(), |_fraction_done| {});
save_image(Path::new("room.png"), &pixels, camera.width, camera.height, 1.0, ToneMap::Aces)?;
```

![A room lit through a doorway](assets/readme/library.png)

`cargo run --release --example library` renders this image.

## Lighting an interior

A path tracer finds the light of an interior by chance: most of the sky is behind walls, the sun
reaches a patch of floor, and that patch lights everything else. The integrator departs from
SORT where that makes noise:

- **A shadow ray per kind of light** (each sun, the sky, the punctual lights, the emissive
  surfaces), not one light picked by its power. Among lamps, and among punctual lights, the one
  that contributes most at the shaded point is favored (resampled importance sampling).
- **Portals.** The sky is sampled through the openings. Thin glass is a portal by itself; an
  opening with no glass is marked with `SceneBuilder::add_portal` or, in a glTF file, with a
  material whose name starts with `portal` (its geometry is not rendered).
- **Sun points.** Each path is given a point lit by the sun through a portal and connects its
  vertices to it, which finds the sunlight bounced off the floor.
- **The ray that continues a path also looks for the lights**, weighted against light sampling,
  where SORT traces a second ray per vertex for that.
- **Blur after a rough bounce** (`--filter-glossy`, 0.5 by default). A polished surface seen
  after a matte bounce is made at least as blurry as that bounce was, so the caustics it would
  cast become soft light, not isolated bright pixels. This is the one change that is not exact:
  it darkens the built-in scene by 2.6%. `--filter-glossy 0` turns it off.

| `--filter-glossy 0`: exact caustics | default |
|---|---|
| ![Bright isolated pixels all over the walls](assets/readme/caustics-exact.png) | ![The same image with no isolated bright pixels](assets/readme/caustics-filtered.png) |

Both at 48 samples per pixel. Portals and sun points only change how light is sampled, not the
result, which a test checks.

On the built-in scene (320x180, depth 8) and for the same render time, the mean relative squared
error against a reference fell from 0.041 to 0.0057, compared with the first version of this
port, which sampled the lights the way SORT does. The tools of that
measurement are in `examples/`: `compare` (error against a reference image) and `variance`
(variance of each pixel, one kind of light at a time).

## What is ported

| SORT (C++) | SORT-rs | Notes |
|---|---|---|
| `integrator/pathtracing`, `integratormethod` | `integrator.rs` | Path tracing, light sampling with multiple importance sampling, Russian roulette; see above |
| `accel/bvh`, `bvh_utils` | `bvh.rs` | SAH with 16 bins; the tree is a flat array, traversed without recursion |
| `shape/triangle` | `scene.rs` | Watertight intersection |
| `camera/perspective` | `camera.rs` | With depth of field |
| `bsdf/disney`, `microfacet`, `fresnel`, `lambert`, `transparent` | `bsdf.rs` | The Disney BRDF without sub-surface scattering; anisotropic GGX |
| `scatteringevent` | `bsdf.rs` | A mix of BXDFs with evaluation and sampling weights |
| `light/distant`, `pointlight`, `spot`, `skylight`, `hdrskylight`, `area` | `light.rs` | An area light is a set of emissive triangles |
| `material` and the Tiny Shading Language | `material.rs` | No shading language: a fixed set of parameters becomes the closures |
| The binary format of the Blender exporter | `gltf_loader.rs` | glTF 2.0 |

## Other differences from SORT

- **Thin glass** (transmission with no `KHR_materials_volume`): light goes straight through, the
  pane reflects as a dielectric, and shadow rays cross it attenuated. That is what lets the sun
  into a room through a window.
- Shadow rays start 0.1 mm from the surface, not 1 cm: at 1 cm, light leaks along every corner
  of a room modelled in meters.
- The BVH may be 48 levels deep, not 16, for scenes of millions of triangles.
- Consistency fixes: rough refraction is zero for directions its own sampling cannot produce,
  the pdf of the diffuse lobe of the Disney BRDF covers one hemisphere only, and the mix of
  BXDFs is evaluated in one frame. They are commented in the code.

## Not ported yet

Participating media, sub-surface scattering, hair and fur, fabric, the MERL and Fourier BRDFs,
coat and double-sided closures, the other integrators (bidirectional, light tracing, instant
radiosity, ambient occlusion, Whitted), the other accelerators (QBVH, OBVH, k-d tree, octree,
uniform grid), SIMD, and the orthographic and environment cameras. On the glTF side: alpha
cutouts, `KHR_texture_transform` and clearcoat.

## Tests

`cargo test` runs 18 tests: importance sampling against value and pdf for every BXDF, the BVH
against a brute force loop, the camera, the environment map, the glTF loader, and renders
compared with analytic results.

## License

SORT-rs is a derived work of SORT, which is Copyright (c) 2011-2023 Jiayin Cao and distributed
under the GNU General Public License version 3. SORT-rs is distributed under the same license,
[GPL-3.0-only](LICENSE).
