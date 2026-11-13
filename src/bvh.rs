// This file is a part of SORT-rs, a Rust port of SORT (Simple Open Ray Tracing).
// Original work Copyright (c) 2011-2023 by Jiayin Cao, GPLv3.

//! Bounding volume hierarchy built with a binned surface area heuristic.
//!
//! Ported from `src/accel/bvh.cpp` and `bvh_utils.h`. The tree is stored in a flat array and
//! traversed iteratively instead of through heap allocated nodes.

use crate::math::{BBox, Ray, Vec3};

const SPLIT_COUNT: usize = 16;
const MAX_PRIMITIVES_IN_LEAF: usize = 8;
// SORT caps the depth at 16, which degenerates into huge leaves on multi-million triangle
// architectural scenes. The traversal stack below is sized after this value.
const MAX_NODE_DEPTH: usize = 48;

#[derive(Clone, Copy)]
struct Node {
    bbox: BBox,
    /// Leaf: offset of the first primitive. Inner node: index of the left child, the right
    /// child is stored right after it.
    offset: u32,
    /// Number of primitives, zero for inner nodes.
    count: u32,
}

struct BuildPrimitive {
    index: u32,
    bbox: BBox,
    centroid: Vec3,
}

pub struct Bvh {
    nodes: Vec<Node>,
    /// Primitive indices, reordered so that each leaf references a contiguous range.
    primitives: Vec<u32>,
    pub depth: usize,
}

impl Bvh {
    pub fn build(bboxes: &[BBox]) -> Self {
        let mut prims: Vec<BuildPrimitive> = bboxes
            .iter()
            .enumerate()
            .map(|(i, b)| BuildPrimitive { index: i as u32, bbox: *b, centroid: b.center() })
            .collect();

        let mut bvh = Bvh { nodes: Vec::new(), primitives: Vec::new(), depth: 0 };
        if !prims.is_empty() {
            bvh.nodes.reserve(prims.len() / 2);
            bvh.nodes.push(Node { bbox: BBox::default(), offset: 0, count: 0 });
            let end = prims.len();
            bvh.split_node(0, &mut prims, 0, end, 1);
        }
        bvh.primitives = prims.iter().map(|p| p.index).collect();
        bvh
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn bbox(&self) -> BBox {
        self.nodes.first().map(|n| n.bbox).unwrap_or_default()
    }

    fn split_node(&mut self, node: usize, prims: &mut [BuildPrimitive], start: usize, end: usize, depth: usize) {
        self.depth = self.depth.max(depth);

        let mut bbox = BBox::default();
        for p in &prims[start..end] {
            bbox.union(&p.bbox);
        }
        self.nodes[node].bbox = bbox;

        let primitive_num = end - start;
        if primitive_num <= MAX_PRIMITIVES_IN_LEAF || depth == MAX_NODE_DEPTH {
            self.make_leaf(node, start, end);
            return;
        }

        let Some((sah, axis, split_pos)) = pick_best_split(&prims[start..end], &bbox) else {
            self.make_leaf(node, start, end);
            return;
        };
        if sah >= primitive_num as f32 {
            self.make_leaf(node, start, end);
            return;
        }

        // partition the primitives
        let slice = &mut prims[start..end];
        let mut mid = 0;
        for i in 0..slice.len() {
            if slice[i].centroid[axis] < split_pos {
                slice.swap(i, mid);
                mid += 1;
            }
        }
        let mid = start + mid;

        // With only 16 candidate planes it is possible to end up with an empty side.
        if mid == start || mid == end {
            self.make_leaf(node, start, end);
            return;
        }

        let left = self.nodes.len();
        let empty = Node { bbox: BBox::default(), offset: 0, count: 0 };
        self.nodes.push(empty);
        self.nodes.push(empty);
        self.nodes[node].offset = left as u32;
        self.split_node(left, prims, start, mid, depth + 1);
        self.split_node(left + 1, prims, mid, end, depth + 1);
    }

    fn make_leaf(&mut self, node: usize, start: usize, end: usize) {
        self.nodes[node].offset = start as u32;
        self.nodes[node].count = (end - start) as u32;
    }

    /// Find the nearest intersection.
    ///
    /// `test` is called with a primitive index and the current nearest distance. It returns the
    /// distance of the intersection if there is one closer than that.
    pub fn intersect<F: FnMut(u32, f32) -> Option<f32>>(&self, ray: &Ray, mut test: F) -> bool {
        let mut tmax = ray.tmax;
        let mut found = false;
        self.traverse(ray, &mut tmax, |prim, tmax| {
            if let Some(t) = test(prim, *tmax) {
                *tmax = t;
                found = true;
            }
            false
        });
        found
    }

    /// Returns true as soon as `test` reports an intersection for any primitive.
    pub fn occluded<F: FnMut(u32) -> bool>(&self, ray: &Ray, mut test: F) -> bool {
        let mut tmax = ray.tmax;
        self.traverse(ray, &mut tmax, |prim, _| test(prim))
    }

    /// Generic ordered traversal. `visit` returns true to terminate the traversal.
    #[inline]
    fn traverse<F: FnMut(u32, &mut f32) -> bool>(&self, ray: &Ray, tmax: &mut f32, mut visit: F) -> bool {
        let Some(root) = self.nodes.first() else {
            return false;
        };
        let fmin = root.bbox.intersect(ray, *tmax);
        if fmin < 0.0 {
            return false;
        }

        let mut stack = [(0u32, 0.0f32); MAX_NODE_DEPTH + 2];
        let mut top = 1;
        stack[0] = (0, fmin);

        while top > 0 {
            top -= 1;
            let (index, fmin) = stack[top];
            if fmin > *tmax {
                continue;
            }
            let node = &self.nodes[index as usize];

            if node.count != 0 {
                let start = node.offset as usize;
                for &prim in &self.primitives[start..start + node.count as usize] {
                    if visit(prim, tmax) {
                        return true;
                    }
                }
                continue;
            }

            let left = node.offset;
            let fmin0 = self.nodes[left as usize].bbox.intersect(ray, *tmax);
            let fmin1 = self.nodes[left as usize + 1].bbox.intersect(ray, *tmax);

            // push the farther child first so that the nearer one is visited first
            let (near, far) = if fmin1 > fmin0 { ((left, fmin0), (left + 1, fmin1)) } else { ((left + 1, fmin1), (left, fmin0)) };
            if far.1 >= 0.0 {
                stack[top] = far;
                top += 1;
            }
            if near.1 >= 0.0 {
                stack[top] = near;
                top += 1;
            }
        }
        false
    }
}

/// Evaluate the SAH value of a split.
#[inline]
fn sah(left: usize, right: usize, lbox: &BBox, rbox: &BBox, parent: &BBox) -> f32 {
    (left as f32 * lbox.half_surface_area() + right as f32 * rbox.half_surface_area())
        / parent.half_surface_area()
}

/// Pick the best split among 16 candidate planes along the widest axis of the centroids.
/// Returns the SAH value, the axis and the position of the plane.
fn pick_best_split(prims: &[BuildPrimitive], node_bbox: &BBox) -> Option<(f32, usize, f32)> {
    let mut inner = BBox::default();
    for p in prims {
        inner.union_point(p.centroid);
    }

    let axis = inner.max_axis();
    let split_start = inner.min[axis];
    let split_delta = (inner.max[axis] - inner.min[axis]) / SPLIT_COUNT as f32;
    if !(split_delta > 0.0) {
        return None;
    }
    let inv_split_delta = 1.0 / split_delta;

    // distribute the primitives into bins
    let mut bin = [0usize; SPLIT_COUNT];
    let mut bbox = [BBox::default(); SPLIT_COUNT];
    for p in prims {
        let index = (((p.centroid[axis] - split_start) * inv_split_delta) as usize).min(SPLIT_COUNT - 1);
        bin[index] += 1;
        bbox[index].union(&p.bbox);
    }

    let mut rbox = [BBox::default(); SPLIT_COUNT - 1];
    rbox[SPLIT_COUNT - 2] = bbox[SPLIT_COUNT - 1];
    for i in (0..SPLIT_COUNT - 2).rev() {
        rbox[i] = rbox[i + 1];
        rbox[i].union(&bbox[i + 1]);
    }

    let mut left = bin[0];
    let mut lbox = bbox[0];
    let mut pos = split_start + split_delta;
    let mut best = (f32::MAX, axis, pos);
    for i in 0..SPLIT_COUNT - 1 {
        let value = sah(left, prims.len() - left, &lbox, &rbox[i], node_bbox);
        if value < best.0 {
            best = (value, axis, pos);
        }
        left += bin[i + 1];
        lbox.union(&bbox[i + 1]);
        pos += split_delta;
    }

    (best.0 < f32::MAX).then_some(best)
}
