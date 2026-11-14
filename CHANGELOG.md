# Changelog

All notable changes to SORT-rs are recorded in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Within a version the entries are
listed newest first.

## [Unreleased]

### Added

- CLI: `--region <x,y,w,h>` renders a rectangle of the image only, in pixels from the top left
  corner. The rest of the image is left black.

## [0.1.1] - 2026-11-14

### Added

- `render::Progressive`: a render refined one sample per pixel at a time, for an application
  that shows the image while it converges. `step` and `step_by` add samples, `image` and
  `region_pixels` read the image so far, and dropping it abandons the render.
- `render::Region` and `Progressive::with_region`: render a rectangle of the image only; the
  rest is never traced.
- `examples/progressive.rs`.

## [0.1.0] - 2026-11-13

The first release.

### Added

- Participating media (`src/medium.rs`): fog and haze that absorb and scatter light, with the
  Henyey-Greenstein phase function. `--fog <density>` fills the bounds of the scene,
  `--fog-height <m>` makes it a layer that goes on forever sideways, `--fog-color` and
  `--fog-anisotropy` set what it scatters and where to. `SceneBuilder::fog` in the library.
- `Material::medium`: what fills a solid transmissive object, so thick tinted glass and water
  darken with the distance light travels in them. The loader reads it from the
  `attenuationColor` and `attenuationDistance` of `KHR_materials_volume`.
- glTF: materials of the specular-glossiness workflow
  (`KHR_materials_pbrSpecularGlossiness`) are loaded and converted approximately to
  metallic-roughness. Files that require the extension were refused before.
- CLI: `--eye <x,y,z>` and `--target <x,y,z>` place the camera, for files without one. The
  log prints the bounds of the scene to help choose the positions.
- Materials: a coat layer over the Disney BRDF, the DreamWorks fabric and the
  distribution-based cloth, sheen, and a back material for double-sided surfaces.
- glTF: alpha modes (mask as holes in the geometry, blend as partial coverage),
  `KHR_texture_transform`, `KHR_materials_clearcoat` and `KHR_materials_sheen`, and
  orthographic cameras.
- Cameras: orthographic and 360 degree latitude-longitude panorama (`--projection`,
  `--ortho-height`).
- Integrator: ambient occlusion for white clay renders (`--integrator ao`, `--ao-distance`).
- `examples/materials.rs`: coated plastic, velvet and the two cloth models over a varnished
  floor.
- `examples/furnished.rs`, three glTF models with their renders, and `SceneBuilder::bounds`.
- The initial port of SORT's path tracer: Disney BRDF, glass, thin glass that lets shadow rays
  through, area, point, spot, distant and sky lights, HDR environment maps, sky portals, a BVH,
  the glTF 2.0 loader, the `sort` command line and the built-in interior demo.

### Changed

- Environment maps wider than 8192 pixels are averaged down by a whole factor when loaded,
  which keeps the energy of the sun and bounds memory.
- The texture fields of `Material` are `Option<TextureMap>` instead of `Option<Arc<Texture>>`,
  to carry the texture transform. An `Arc<Texture>` converts with `.into()`.
- `Scene::sun_point_density` takes the `&Hit` of the surface.
- A scattering event holds up to 6 lobes (`MAX_BXDF_COUNT`).
- `Texture::texel` is public.
- `integrator::evaluate_direct` takes the position of the vertex and an optional `&Hit`, a
  vertex inside a medium having no surface.
- `Scene::transmittance` includes the fog; `Scene::surface_transmittance` is what it was.
- glTF solids with a `KHR_materials_volume` attenuation render darker than before, the
  attenuation was ignored.

### Fixed

- Large HDR environment maps (a 10000x5000 `.hdr`, for instance) failed to load with "Memory
  limit exceeded".
- `--projection ortho` on a perspective glTF camera showed a view about 0.7 m high, because
  glTF cameras have no focal distance. The height now comes from the distance to the center of
  the scene.

[Unreleased]: https://github.com/francisreboucas/sort-raytracing/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/francisreboucas/sort-raytracing/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/francisreboucas/sort-raytracing/releases/tag/v0.1.0
