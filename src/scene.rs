// This file is a part of SORT-rs, a Rust port of SORT (Simple Open Ray Tracing).
// Original work Copyright (c) 2011-2023 by Jiayin Cao, GPLv3.

//! The scene: world space triangles, materials, lights and the acceleration structure.

use crate::bsdf::ScatteringEvent;
use crate::bvh::Bvh;
use crate::light::Light;
use crate::material::Material;
use crate::math::*;

/// Surface interaction, the result of a ray hitting the scene.
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub t: f32,
    pub p: Vec3,
    /// Geometric normal, on the counter-clockwise side of the triangle.
    pub ng: Vec3,
    /// Interpolated shading normal, on the same side as `ng`.
    pub n: Vec3,
    pub tangent: Vec3,
    /// Handedness of the tangent frame, +1 or -1.
    pub tangent_sign: f32,
    pub uv: Vec2,
    pub tri: u32,
    pub material: u32,
}

/// Portals almost parallel to the sun rays are ignored, the density of the points they give
/// blows up.
const PORTAL_MIN_COS: f32 = 1e-3;

/// A surface point lit by a sun through a portal.
pub struct SunPoint {
    pub hit: Hit,
    /// Direction the sun light travels.
    pub dir: Vec3,
    /// Irradiance on a surface facing the sun, after the glass it crossed.
    pub irradiance: Spectrum,
    /// Density w.r.t. surface area with which the point was generated.
    pub density: f32,
}

/// Collects geometry, materials and lights before the acceleration structure is built.
#[derive(Default)]
pub struct SceneBuilder {
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    uvs: Vec<Vec2>,
    tangents: Vec<Vec4>,
    tris: Vec<[u32; 3]>,
    tri_material: Vec<u32>,
    pub materials: Vec<Material>,
    pub lights: Vec<Light>,
    portals: Vec<[Vec3; 3]>,
}

/// Number of light candidates considered at each path vertex, see `LightGroup`.
pub const LIGHT_CANDIDATES: usize = 8;

/// A set of lights that direct lighting draws its candidates from. A small group is
/// enumerated, every light giving one candidate; a large one gives `LIGHT_CANDIDATES`
/// candidates picked at random with `distribution`.
pub struct LightGroup {
    /// Indices into `Scene::lights`.
    pub ids: Vec<u32>,
    pub distribution: Distribution1D,
    pub enumerate: bool,
}

/// Openings that sky light enters an interior through.
struct Portals {
    tris: Vec<[Vec3; 3]>,
    normals: Vec<Vec3>,
    /// Picks a triangle proportionally to its area.
    distribution: Distribution1D,
    total_area: f32,
    bvh: Bvh,
}

impl SceneBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_material(&mut self, material: Material) -> u32 {
        self.materials.push(material);
        self.materials.len() as u32 - 1
    }

    pub fn add_light(&mut self, light: Light) {
        self.lights.push(light);
    }

    pub fn triangle_count(&self) -> usize {
        self.tris.len()
    }

    /// Add a triangle mesh in world space. Missing normals and tangents are generated.
    pub fn add_mesh(
        &mut self,
        positions: &[Vec3],
        normals: Option<&[Vec3]>,
        uvs: Option<&[Vec2]>,
        tangents: Option<&[Vec4]>,
        indices: &[[u32; 3]],
        material: u32,
    ) {
        let base = self.positions.len() as u32;
        let count = positions.len();
        let valid = |t: &&[u32; 3]| t.iter().all(|&i| (i as usize) < count);

        // portal geometry is not rendered, it only guides the sampling of the sky
        if self.materials.get(material as usize).is_some_and(|m| m.portal) {
            for t in indices.iter().filter(valid) {
                self.portals.push(t.map(|i| positions[i as usize]));
            }
            return;
        }

        let normals = match normals {
            Some(n) if n.len() == count => n.to_vec(),
            _ => {
                // area weighted vertex normals
                let mut n = vec![Vec3::ZERO; count];
                for t in indices.iter().filter(valid) {
                    let [a, b, c] = t.map(|i| positions[i as usize]);
                    let face = (b - a).cross(c - a);
                    for &i in t {
                        n[i as usize] += face;
                    }
                }
                n
            }
        };
        let uvs = match uvs {
            Some(uv) if uv.len() == count => uv.to_vec(),
            _ => vec![Vec2::ZERO; count],
        };
        let tangents = match tangents {
            Some(t) if t.len() == count => t.to_vec(),
            _ => generate_tangents(positions, &normals, &uvs, indices),
        };

        self.positions.extend_from_slice(positions);
        self.normals.extend(normals.iter().map(|n| n.normalize_or_zero()));
        self.uvs.extend_from_slice(&uvs);
        self.tangents.extend_from_slice(&tangents);
        for t in indices.iter().filter(valid) {
            self.tris.push(t.map(|i| i + base));
            self.tri_material.push(material);
        }
    }

    /// Add a quad as two triangles, `p0..p3` in counter-clockwise order seen from the front.
    pub fn add_quad(&mut self, p: [Vec3; 4], material: u32) {
        let uvs = [Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0), Vec2::new(1.0, 1.0), Vec2::new(0.0, 1.0)];
        self.add_mesh(&p, None, Some(&uvs), None, &[[0, 1, 2], [0, 2, 3]], material);
    }

    /// Mark an opening (a window, a door, a skylight) that sky light comes through, as a quad
    /// covering it. Portals are invisible; they only tell the renderer where to look for the
    /// sky from inside a room, which removes most of the noise of sky lit interiors. Thin
    /// glass is a portal already, so glazed windows need nothing.
    pub fn add_portal(&mut self, p: [Vec3; 4]) {
        self.portals.push([p[0], p[1], p[2]]);
        self.portals.push([p[0], p[2], p[3]]);
    }

    /// Add an axis aligned box with outward facing normals.
    pub fn add_box(&mut self, min: Vec3, max: Vec3, material: u32) {
        let c = |x: bool, y: bool, z: bool| {
            Vec3::new(if x { max.x } else { min.x }, if y { max.y } else { min.y }, if z { max.z } else { min.z })
        };
        let (f, t) = (false, true);
        self.add_quad([c(f, f, t), c(t, f, t), c(t, t, t), c(f, t, t)], material); // +z
        self.add_quad([c(t, f, f), c(f, f, f), c(f, t, f), c(t, t, f)], material); // -z
        self.add_quad([c(t, f, t), c(t, f, f), c(t, t, f), c(t, t, t)], material); // +x
        self.add_quad([c(f, f, f), c(f, f, t), c(f, t, t), c(f, t, f)], material); // -x
        self.add_quad([c(f, t, t), c(t, t, t), c(t, t, f), c(f, t, f)], material); // +y
        self.add_quad([c(f, f, f), c(t, f, f), c(t, f, t), c(f, f, t)], material); // -y
    }

    pub fn build(mut self) -> Scene {
        if self.materials.is_empty() {
            self.materials.push(Material::default());
        }
        let last_material = self.materials.len() as u32 - 1;
        for m in self.tri_material.iter_mut() {
            *m = (*m).min(last_material);
        }

        let bboxes: Vec<BBox> = self
            .tris
            .iter()
            .map(|t| {
                let mut b = BBox::default();
                for &i in t {
                    b.union_point(self.positions[i as usize]);
                }
                b
            })
            .collect();
        let bvh = Bvh::build(&bboxes);
        let bbox = bvh.bbox();

        let has_transparency = self.materials.iter().any(|m| m.thin && m.transmission > 0.0);
        let mut scene = Scene {
            positions: self.positions,
            normals: self.normals,
            uvs: self.uvs,
            tangents: self.tangents,
            tris: self.tris,
            tri_material: self.tri_material,
            materials: self.materials,
            lights: self.lights,
            light_groups: Vec::new(),
            light_scale: Vec::new(),
            tri_light: Vec::new(),
            sky: None,
            portals: None,
            suns: Vec::new(),
            sun_distribution: Distribution1D::default(),
            sun_pick: Vec::new(),
            world_radius: 1.0,
            bvh,
            bbox,
            has_transparency,
        };

        // every emissive triangle is an area light
        scene.tri_light = vec![u32::MAX; scene.tris.len()];
        for tri in 0..scene.tris.len() as u32 {
            if scene.triangle_material(tri).is_emissive() {
                let [p0, p1, p2] = scene.triangle_positions(tri);
                if (p1 - p0).cross(p2 - p0).length_squared() > 0.0 {
                    scene.tri_light[tri as usize] = scene.lights.len() as u32;
                    scene.lights.push(Light::Area { tri });
                }
            }
        }
        scene.sky = scene.lights.iter().position(|l| matches!(l, Light::Sky(_)));

        // Lights are split in two groups, emissive triangles and everything else, so that a
        // ceiling full of emissive triangles never starves the sun and the sky of samples.
        let world_radius = if bbox.is_empty() { 1.0 } else { (bbox.max - bbox.min).length() * 0.5 };
        scene.world_radius = world_radius;
        scene.light_scale = vec![1.0; scene.lights.len()];

        scene.suns = (0..scene.lights.len() as u32)
            .filter(|&i| matches!(scene.lights[i as usize], Light::Distant { .. }))
            .collect();
        let sun_power: Vec<f32> =
            scene.suns.iter().map(|&i| scene.lights[i as usize].power(&scene, world_radius).max(1e-6)).collect();
        scene.sun_distribution = Distribution1D::new(&sun_power);
        scene.sun_pick = vec![0.0; scene.lights.len()];
        for (k, &i) in scene.suns.iter().enumerate() {
            scene.sun_pick[i as usize] = scene.sun_distribution.discrete_pdf(k);
        }
        for area in [false, true] {
            let ids: Vec<u32> = (0..scene.lights.len() as u32)
                .filter(|&i| matches!(scene.lights[i as usize], Light::Area { .. }) == area)
                .collect();
            if ids.is_empty() {
                continue;
            }
            // half proportional to the power, half uniform: no light is ever left out
            let power: Vec<f32> =
                ids.iter().map(|&i| scene.lights[i as usize].power(&scene, world_radius).max(0.0)).collect();
            let total: f32 = power.iter().sum();
            let uniform = 1.0 / ids.len() as f32;
            let pmf: Vec<f32> = power
                .iter()
                .map(|p| if total > 0.0 { 0.5 * p / total + 0.5 * uniform } else { uniform })
                .collect();
            let distribution = Distribution1D::new(&pmf);
            let enumerate = ids.len() <= LIGHT_CANDIDATES;
            if !enumerate {
                for (k, &i) in ids.iter().enumerate() {
                    scene.light_scale[i as usize] = LIGHT_CANDIDATES as f32 * distribution.discrete_pdf(k);
                }
            }
            scene.light_groups.push(LightGroup { ids, distribution, enumerate });
        }

        // Portals only matter when there is a sky or a sun behind them. Thin glass is a portal
        // too.
        if scene.sky.is_some() || !scene.suns.is_empty() {
            let mut tris = self.portals;
            for tri in 0..scene.tris.len() as u32 {
                let material = scene.triangle_material(tri);
                if material.thin && material.transmission > 0.0 {
                    tris.push(scene.triangle_positions(tri));
                }
            }
            tris.retain(|[p0, p1, p2]| (*p1 - *p0).cross(*p2 - *p0).length_squared() > 0.0);
            if !tris.is_empty() {
                let areas: Vec<f32> = tris.iter().map(|[p0, p1, p2]| (*p1 - *p0).cross(*p2 - *p0).length() * 0.5).collect();
                let bboxes: Vec<BBox> = tris
                    .iter()
                    .map(|t| {
                        let mut b = BBox::default();
                        t.iter().for_each(|p| b.union_point(*p));
                        b
                    })
                    .collect();
                scene.portals = Some(Portals {
                    normals: tris.iter().map(|[p0, p1, p2]| (*p1 - *p0).cross(*p2 - *p0).normalize()).collect(),
                    distribution: Distribution1D::new(&areas),
                    total_area: areas.iter().sum(),
                    bvh: Bvh::build(&bboxes),
                    tris,
                });
            }
        }
        scene
    }
}

/// Per-vertex tangents from the texture coordinates, with an arbitrary fallback.
fn generate_tangents(positions: &[Vec3], normals: &[Vec3], uvs: &[Vec2], indices: &[[u32; 3]]) -> Vec<Vec4> {
    let count = positions.len();
    let mut tan = vec![Vec3::ZERO; count];
    let mut bitan = vec![Vec3::ZERO; count];
    for t in indices {
        if t.iter().any(|&i| i as usize >= count) {
            continue;
        }
        let [i0, i1, i2] = t.map(|i| i as usize);
        let (e1, e2) = (positions[i1] - positions[i0], positions[i2] - positions[i0]);
        let (d1, d2) = (uvs[i1] - uvs[i0], uvs[i2] - uvs[i0]);
        let det = d1.x * d2.y - d2.x * d1.y;
        if det.abs() < 1e-12 {
            continue;
        }
        let r = 1.0 / det;
        let tangent = (e1 * d2.y - e2 * d1.y) * r;
        let bitangent = (e2 * d1.x - e1 * d2.x) * r;
        for i in [i0, i1, i2] {
            tan[i] += tangent;
            bitan[i] += bitangent;
        }
    }
    (0..count)
        .map(|i| {
            let n = normals[i].normalize_or_zero();
            let t = (tan[i] - n * n.dot(tan[i])).normalize_or_zero();
            if t == Vec3::ZERO || n == Vec3::ZERO {
                let fallback = if n == Vec3::ZERO { Vec3::X } else { coordinate_system(n).0 };
                return fallback.extend(1.0);
            }
            let sign = if n.cross(t).dot(bitan[i]) < 0.0 { -1.0 } else { 1.0 };
            t.extend(sign)
        })
        .collect()
}

pub struct Scene {
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    uvs: Vec<Vec2>,
    tangents: Vec<Vec4>,
    tris: Vec<[u32; 3]>,
    tri_material: Vec<u32>,
    pub materials: Vec<Material>,
    pub lights: Vec<Light>,
    light_groups: Vec<LightGroup>,
    /// Expected number of candidates each light gives per path vertex.
    light_scale: Vec<f32>,
    /// Light index of each emissive triangle, `u32::MAX` for the others.
    tri_light: Vec<u32>,
    sky: Option<usize>,
    portals: Option<Portals>,
    /// Distant lights (suns), with the distribution one is picked from and the probability of
    /// each light of the scene being picked (zero for the lights that are not distant).
    suns: Vec<u32>,
    sun_distribution: Distribution1D,
    sun_pick: Vec<f32>,
    world_radius: f32,
    bvh: Bvh,
    pub bbox: BBox,
    has_transparency: bool,
}

/// Watertight ray-triangle intersection, ported from `Triangle::GetIntersect`.
/// Returns the distance and the barycentric weights of the second and third vertices.
#[inline]
fn intersect_triangle(ray: &Ray, p0: Vec3, p1: Vec3, p2: Vec3, tmax: f32) -> Option<(f32, f32, f32)> {
    let [lx, ly, lz] = ray.local;
    // translate to the ray origin, permute so that the major axis is y, then shear so that the
    // ray direction points to ( 0 , 1 , 0 )
    let transform = |p: Vec3| {
        let q = p - ray.ori;
        let q = Vec3::new(q[lx], q[ly], q[lz]);
        Vec3::new(q.x + ray.scale.x * q.y, q.y, q.z + ray.scale.z * q.y)
    };
    let (a, b, c) = (transform(p0), transform(p1), transform(p2));

    // edge functions
    let mut e0 = b.x * c.z - b.z * c.x;
    let mut e1 = c.x * a.z - c.z * a.x;
    let mut e2 = a.x * b.z - a.z * b.x;

    // fall back to double precision for better accuracy at some performance cost
    if e0 == 0.0 || e1 == 0.0 || e2 == 0.0 {
        e0 = (b.x as f64 * c.z as f64 - b.z as f64 * c.x as f64) as f32;
        e1 = (c.x as f64 * a.z as f64 - c.z as f64 * a.x as f64) as f32;
        e2 = (a.x as f64 * b.z as f64 - a.z as f64 * b.x as f64) as f32;
    }

    if (e0 < 0.0 || e1 < 0.0 || e2 < 0.0) && (e0 > 0.0 || e1 > 0.0 || e2 > 0.0) {
        return None;
    }
    let det = e0 + e1 + e2;
    if det == 0.0 {
        return None;
    }

    let inv_det = 1.0 / det;
    let t = (e0 * a.y + e1 * b.y + e2 * c.y) * ray.scale.y * inv_det;
    if !(t > ray.tmin && t < tmax) {
        return None;
    }
    Some((t, e1 * inv_det, e2 * inv_det))
}

impl Scene {
    pub fn triangle_count(&self) -> usize {
        self.tris.len()
    }

    pub fn bvh_stats(&self) -> (usize, usize) {
        (self.bvh.node_count(), self.bvh.depth)
    }

    #[inline]
    pub fn triangle_positions(&self, tri: u32) -> [Vec3; 3] {
        self.tris[tri as usize].map(|i| self.positions[i as usize])
    }

    #[inline]
    pub fn triangle_uvs(&self, tri: u32) -> [Vec2; 3] {
        self.tris[tri as usize].map(|i| self.uvs[i as usize])
    }

    #[inline]
    pub fn triangle_uv(&self, tri: u32, b1: f32, b2: f32) -> Vec2 {
        let [uv0, uv1, uv2] = self.triangle_uvs(tri);
        uv0 * (1.0 - b1 - b2) + uv1 * b1 + uv2 * b2
    }

    #[inline]
    pub fn triangle_material(&self, tri: u32) -> &Material {
        &self.materials[self.tri_material[tri as usize] as usize]
    }

    /// Geometric normal of a triangle.
    #[inline]
    pub fn triangle_normal(&self, tri: u32) -> Vec3 {
        let [p0, p1, p2] = self.triangle_positions(tri);
        (p1 - p0).cross(p2 - p0).normalize_or_zero()
    }

    #[inline]
    pub fn intersect_triangle(&self, tri: u32, ray: &Ray, tmax: f32) -> Option<(f32, f32, f32)> {
        let [p0, p1, p2] = self.triangle_positions(tri);
        intersect_triangle(ray, p0, p1, p2, tmax)
    }

    /// Nearest intersection as a triangle index, distance and barycentric weights.
    #[inline]
    fn nearest(&self, ray: &Ray) -> Option<(u32, f32, f32, f32)> {
        let mut best = None;
        self.bvh.intersect(ray, |tri, tmax| {
            let (t, b1, b2) = self.intersect_triangle(tri, ray, tmax)?;
            best = Some((tri, t, b1, b2));
            Some(t)
        });
        best
    }

    /// Get the nearest intersection between a ray and the scene.
    pub fn intersect(&self, ray: &Ray) -> Option<Hit> {
        let (tri, t, b1, b2) = self.nearest(ray)?;
        let b0 = 1.0 - b1 - b2;
        let idx = self.tris[tri as usize].map(|i| i as usize);
        let [p0, p1, p2] = idx.map(|i| self.positions[i]);

        let ng = (p1 - p0).cross(p2 - p0).normalize_or_zero();
        let mut n = (self.normals[idx[0]] * b0 + self.normals[idx[1]] * b1 + self.normals[idx[2]] * b2)
            .normalize_or_zero();
        if n == Vec3::ZERO {
            n = ng;
        } else if n.dot(ng) < 0.0 {
            n = -n;
        }
        let tangent = self.tangents[idx[0]] * b0 + self.tangents[idx[1]] * b1 + self.tangents[idx[2]] * b2;

        Some(Hit {
            t,
            p: p0 * b0 + p1 * b1 + p2 * b2,
            ng,
            n,
            tangent: tangent.truncate(),
            tangent_sign: if self.tangents[idx[0]].w < 0.0 { -1.0 } else { 1.0 },
            uv: self.uvs[idx[0]] * b0 + self.uvs[idx[1]] * b1 + self.uvs[idx[2]] * b2,
            tri,
            material: self.tri_material[tri as usize],
        })
    }

    /// Whether anything blocks the ray, ignoring transparency.
    pub fn is_occluded(&self, ray: &Ray) -> bool {
        self.bvh.occluded(ray, |tri| self.intersect_triangle(tri, ray, ray.tmax).is_some())
    }

    /// Fraction of light that travels along a shadow ray. Thin transmissive surfaces attenuate
    /// the light instead of blocking it (`Visibility::GetAttenuation` with transparent shadows).
    pub fn transmittance(&self, ray: &Ray) -> Spectrum {
        if !self.has_transparency {
            return if self.is_occluded(ray) { Spectrum::ZERO } else { Spectrum::ONE };
        }

        let mut attenuation = Spectrum::ONE;
        let mut ray = *ray;
        for _ in 0..32 {
            let Some((tri, t, b1, b2)) = self.nearest(&ray) else {
                return attenuation;
            };
            match self.triangle_material(tri).shadow_transmittance(self.triangle_uv(tri, b1, b2)) {
                Some(tr) => attenuation *= tr,
                None => return Spectrum::ZERO,
            }
            if is_black(attenuation) {
                return Spectrum::ZERO;
            }
            ray.tmin = t + t.abs() * 1e-5 + 1e-5;
        }
        Spectrum::ZERO
    }

    /// The groups direct lighting draws its light candidates from.
    #[inline]
    pub fn light_groups(&self) -> &[LightGroup] {
        &self.light_groups
    }

    /// Expected number of candidates a light gives per path vertex: one when its group is
    /// enumerated, `LIGHT_CANDIDATES` times its pick probability otherwise.
    #[inline]
    pub fn light_scale(&self, light: u32) -> f32 {
        self.light_scale[light as usize]
    }

    #[inline]
    pub fn has_portals(&self) -> bool {
        self.portals.is_some()
    }

    pub fn portal_count(&self) -> usize {
        self.portals.as_ref().map_or(0, |p| p.tris.len())
    }

    /// Sample a direction from `p` towards a uniformly distributed point of the portals.
    pub fn sample_portal(&self, p: Vec3, pick: f32, u: (f32, f32)) -> Option<Vec3> {
        let portals = self.portals.as_ref()?;
        let (k, _) = portals.distribution.sample_discrete(pick)?;
        let [p0, p1, p2] = portals.tris[k];
        let su = u.0.sqrt();
        let (b0, b1) = (1.0 - su, u.1 * su);
        let dir = (p0 * b0 + p1 * b1 + p2 * (1.0 - b0 - b1) - p).normalize_or_zero();
        (dir != Vec3::ZERO).then_some(dir)
    }

    /// Pdf w.r.t. solid angle of `sample_portal` generating direction `wi` from `p`. Every
    /// portal crossed by the direction counts, which handles double glazing and overlaps.
    pub fn portal_pdf(&self, p: Vec3, wi: Vec3) -> f32 {
        let Some(portals) = &self.portals else {
            return 0.0;
        };
        let ray = Ray::new(p, wi, 0.0, f32::MAX);
        let mut pdf = 0.0;
        portals.bvh.occluded(&ray, |k| {
            let [p0, p1, p2] = portals.tris[k as usize];
            if let Some((t, _, _)) = intersect_triangle(&ray, p0, p1, p2, f32::MAX) {
                let cos = wi.dot(portals.normals[k as usize]).abs();
                if cos > 1e-6 {
                    pdf += t * t / (portals.total_area * cos);
                }
            }
            false
        });
        pdf
    }

    /// Whether sun light enters through portals, see `sample_sun_point`.
    #[inline]
    pub fn has_sun_portals(&self) -> bool {
        self.portals.is_some() && !self.suns.is_empty()
    }

    /// First opaque surface along a ray, with the attenuation of the thin glass crossed
    /// before reaching it.
    fn first_opaque_hit(&self, ray: &Ray) -> Option<(Hit, Spectrum)> {
        let mut attenuation = Spectrum::ONE;
        let mut ray = *ray;
        for _ in 0..32 {
            let hit = self.intersect(&ray)?;
            match self.materials[hit.material as usize].shadow_transmittance(hit.uv) {
                None => return Some((hit, attenuation)),
                Some(tr) => attenuation *= tr,
            }
            if is_black(attenuation) {
                return None;
            }
            ray.tmin = hit.t + hit.t.abs() * 1e-5 + 1e-5;
        }
        None
    }

    /// Density w.r.t. surface area with which `sample_sun_point` generates point `y`, whose
    /// geometric normal is `ng`, for the light travelling along `dir`: every portal crossed by
    /// the line through `y` along `dir` is a way of generating it.
    fn portal_line_density(&self, y: Vec3, ng: Vec3, dir: Vec3) -> f32 {
        let Some(portals) = &self.portals else {
            return 0.0;
        };
        let cos_y = ng.dot(dir).abs();
        let mut density = 0.0;
        for d in [dir, -dir] {
            let ray = Ray::new(y, d, 0.0, f32::MAX);
            portals.bvh.occluded(&ray, |k| {
                let [p0, p1, p2] = portals.tris[k as usize];
                let cos_portal = dir.dot(portals.normals[k as usize]).abs();
                if cos_portal > PORTAL_MIN_COS && intersect_triangle(&ray, p0, p1, p2, f32::MAX).is_some() {
                    density += cos_y / (portals.total_area * cos_portal);
                }
                false
            });
        }
        density
    }

    /// Density w.r.t. surface area with which `sample_sun_point` generates point `y` lit by
    /// distant light `light`. Zero when the light is not a sun or no portal lines up with it.
    pub fn sun_point_density(&self, light: u32, y: Vec3, ng: Vec3) -> f32 {
        let pick = self.sun_pick[light as usize];
        match &self.lights[light as usize] {
            Light::Distant { dir, .. } if pick > 0.0 => pick * self.portal_line_density(y, ng, *dir),
            _ => 0.0,
        }
    }

    /// Sample a surface point lit by a sun through a portal: pick a sun and a point of the
    /// portals, and follow the sun ray that goes through that point down to the first opaque
    /// surface.
    pub fn sample_sun_point(&self, pick_sun: f32, pick_portal: f32, u: (f32, f32)) -> Option<SunPoint> {
        let portals = self.portals.as_ref()?;
        let (s, _) = self.sun_distribution.sample_discrete(pick_sun)?;
        let light = self.suns[s];
        let Light::Distant { dir, intensity } = &self.lights[light as usize] else {
            return None;
        };
        let (k, _) = portals.distribution.sample_discrete(pick_portal)?;
        if dir.dot(portals.normals[k]).abs() <= PORTAL_MIN_COS {
            return None;
        }
        let [p0, p1, p2] = portals.tris[k];
        let su = u.0.sqrt();
        let (b0, b1) = (1.0 - su, u.1 * su);
        let q = p0 * b0 + p1 * b1 + p2 * (1.0 - b0 - b1);

        // start outside of the scene, something may shade the portal
        let back = 2.0 * self.world_radius + (q - self.bbox.center()).length();
        let (hit, attenuation) = self.first_opaque_hit(&Ray::new(q - *dir * back, *dir, 0.0, f32::MAX))?;
        let density = self.sun_point_density(light, hit.p, hit.ng);
        (density > 0.0).then(|| SunPoint { hit, dir: *dir, irradiance: *intensity * attenuation, density })
    }

    /// The sky light, if any.
    #[inline]
    pub fn sky(&self) -> Option<&crate::light::Sky> {
        match self.sky.map(|i| &self.lights[i]) {
            Some(Light::Sky(sky)) => Some(sky),
            _ => None,
        }
    }

    /// Pdf w.r.t. solid angle of the directions sky sampling generates from `p`: one sample
    /// of the sky itself plus, when there are portals, one aimed at them.
    #[inline]
    pub fn sky_sampling_pdf(&self, sky: &crate::light::Sky, p: Vec3, wi: Vec3) -> f32 {
        sky.pdf(wi) + self.portal_pdf(p, wi)
    }

    /// Density with which direct lighting at `p` samples direction `wi` towards the sky.
    /// Used to weight the sky found by BSDF sampling.
    pub fn sky_density(&self, p: Vec3, wi: Vec3) -> f32 {
        match (self.sky, self.sky()) {
            (Some(i), Some(sky)) => self.light_scale[i] * self.sky_sampling_pdf(sky, p, wi),
            _ => 0.0,
        }
    }

    /// Density with which direct lighting at `p` samples the point of an emissive triangle
    /// that a ray leaving `p` in direction `wi` hit. Used to weight emitters found by BSDF
    /// sampling.
    pub fn area_light_density(&self, hit: &Hit, p: Vec3, wi: Vec3) -> f32 {
        let light = self.tri_light[hit.tri as usize];
        if light == u32::MAX {
            return 0.0;
        }
        let [p0, p1, p2] = self.triangle_positions(hit.tri);
        let area = (p1 - p0).cross(p2 - p0).length() * 0.5;
        let cos = wi.dot(hit.ng).abs();
        if area <= 0.0 || cos <= 0.0 {
            return 0.0;
        }
        self.light_scale[light as usize] * (hit.p - p).length_squared() / (area * cos)
    }

    /// Radiance of the sky along a ray that leaves the scene.
    pub fn le(&self, ray: &Ray) -> Spectrum {
        match self.sky.map(|i| &self.lights[i]) {
            Some(Light::Sky(sky)) => sky.evaluate(ray.dir),
            _ => Spectrum::ZERO,
        }
    }

    /// Radiance emitted by the surface at a hit towards `wo`.
    pub fn emission(&self, hit: &Hit, wo: Vec3) -> Spectrum {
        let material = &self.materials[hit.material as usize];
        if !material.is_emissive() || (!material.double_sided && hit.ng.dot(wo) <= 0.0) {
            return Spectrum::ZERO;
        }
        material.emission(hit.uv)
    }

    /// Build the scattering event at a hit: shading frame, normal mapping and the BXDFs.
    /// `min_alpha` widens the specular lobes, see `Material::update_scattering_event`.
    pub fn scattering_event(&self, hit: &Hit, wo: Vec3, min_alpha: f32) -> ScatteringEvent {
        let material = &self.materials[hit.material as usize];

        // Surfaces without an interior are shaded on both sides.
        let mut n = hit.n;
        if !material.has_interior() && hit.ng.dot(wo) < 0.0 {
            n = -n;
        }

        let mut tangent = hit.tangent;
        if let Some(nt) = material.normal_at(hit.uv) {
            let t = (tangent - n * n.dot(tangent)).normalize_or_zero();
            if t != Vec3::ZERO {
                let b = n.cross(t) * hit.tangent_sign;
                let mapped = (t * nt.x + b * nt.y + n * nt.z).normalize_or_zero();
                if mapped != Vec3::ZERO {
                    n = mapped;
                    tangent = t;
                }
            }
        }

        let mut se = ScatteringEvent::new(n, tangent);
        material.update_scattering_event(&mut se, hit.uv, min_alpha);
        se
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    /// The BVH must agree with a brute force loop over all triangles.
    #[test]
    fn bvh_matches_brute_force() {
        let mut rng = Rng::new(42, 1);
        let mut builder = SceneBuilder::new();
        let material = builder.add_material(Material::default());
        let v = |rng: &mut Rng, s: f32| (Vec3::new(rng.f32(), rng.f32(), rng.f32()) - 0.5) * s;
        for _ in 0..3000 {
            let c = v(&mut rng, 20.0);
            let p = [c + v(&mut rng, 1.5), c + v(&mut rng, 1.5), c + v(&mut rng, 1.5)];
            builder.add_mesh(&p, None, None, None, &[[0, 1, 2]], material);
        }
        let scene = builder.build();
        assert_eq!(scene.triangle_count(), 3000);

        let mut hits = 0;
        for _ in 0..3000 {
            let ori = v(&mut rng, 30.0);
            let dir = v(&mut rng, 1.0).normalize();
            let ray = Ray::new(ori, dir, 0.0, f32::MAX);

            let mut expected: Option<(u32, f32)> = None;
            for tri in 0..scene.triangle_count() as u32 {
                let tmax = expected.map_or(f32::MAX, |e| e.1);
                if let Some((t, _, _)) = scene.intersect_triangle(tri, &ray, tmax) {
                    expected = Some((tri, t));
                }
            }

            let hit = scene.intersect(&ray);
            assert_eq!(hit.is_some(), expected.is_some());
            assert_eq!(scene.is_occluded(&ray), expected.is_some());
            if let (Some(hit), Some((_, t))) = (hit, expected) {
                assert!((hit.t - t).abs() <= 1e-4 * t.max(1.0), "{} vs {}", hit.t, t);
                assert!((ray.at(hit.t) - hit.p).length() < 1e-2);
                hits += 1;
            }
        }
        assert!(hits > 100, "too few rays hit anything: {hits}");
    }

    #[test]
    fn axis_aligned_rays_hit_a_quad() {
        let mut builder = SceneBuilder::new();
        let material = builder.add_material(Material::default());
        builder.add_quad(
            [Vec3::new(-1.0, 0.0, 1.0), Vec3::new(1.0, 0.0, 1.0), Vec3::new(1.0, 0.0, -1.0), Vec3::new(-1.0, 0.0, -1.0)],
            material,
        );
        let scene = builder.build();
        // straight down, including right on the shared diagonal edge
        for (x, z) in [(0.3, -0.2), (0.0, 0.0), (0.5, 0.5), (-0.99, 0.99)] {
            let ray = Ray::new(Vec3::new(x, 2.0, z), -Vec3::Y, 0.0, f32::MAX);
            let hit = scene.intersect(&ray).expect("ray must hit the quad");
            assert!((hit.t - 2.0).abs() < 1e-5);
            assert!((hit.ng - Vec3::Y).length() < 1e-5);
        }
        assert!(scene.intersect(&Ray::new(Vec3::new(1.5, 2.0, 0.0), -Vec3::Y, 0.0, f32::MAX)).is_none());
    }
}
