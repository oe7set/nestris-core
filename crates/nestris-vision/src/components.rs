//! Connected components with stats (8-connectivity), replicating the
//! *observable* behavior of `cv2.connectedComponentsWithStats`: the component
//! set with per-component area/bbox/centroid. Label IDs follow our own
//! scan order, not OpenCV's block-based internals — every engine consumer
//! selects components by their stats, never by raw label value.

use crate::image::Image;

/// One connected component of nonzero pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Component {
    pub area: u32,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub cx: f64,
    pub cy: f64,
}

/// Labeled output: `labels` is row-major with 0 = background, 1.. = components
/// (indices into `components` + 1).
pub struct Labeled {
    pub labels: Vec<u32>,
    pub components: Vec<Component>,
    pub width: usize,
    pub height: usize,
}

impl Labeled {
    /// Pixel coordinates of one component (for hull/minAreaRect consumers).
    pub fn component_points(&self, index: usize) -> Vec<(u32, u32)> {
        let want = index as u32 + 1;
        let mut pts = Vec::new();
        for y in 0..self.height {
            for x in 0..self.width {
                if self.labels[y * self.width + x] == want {
                    pts.push((x as u32, y as u32));
                }
            }
        }
        pts
    }

    /// Pixel coordinates for a selected subset of components, collected in
    /// ONE pass over the label image instead of one full scan per component.
    /// Per-component point order matches [`Labeled::component_points`]
    /// (row-major scan order), so downstream consumers are byte-identical.
    pub fn component_points_multi(&self, indices: &[usize]) -> Vec<Vec<(u32, u32)>> {
        // Label value -> output slot + 1 (0 = not requested).
        let mut slot = vec![0u32; self.components.len() + 1];
        for (out_i, &idx) in indices.iter().enumerate() {
            slot[idx + 1] = out_i as u32 + 1;
        }
        let mut out: Vec<Vec<(u32, u32)>> = indices
            .iter()
            .map(|&idx| Vec::with_capacity(self.components[idx].area as usize))
            .collect();
        for y in 0..self.height {
            for x in 0..self.width {
                let s = slot[self.labels[y * self.width + x] as usize];
                if s != 0 {
                    out[(s - 1) as usize].push((x as u32, y as u32));
                }
            }
        }
        out
    }
}

struct UnionFind {
    parent: Vec<u32>,
}

impl UnionFind {
    fn new() -> Self {
        Self { parent: vec![0] }
    }

    fn make(&mut self) -> u32 {
        let id = self.parent.len() as u32;
        self.parent.push(id);
        id
    }

    fn find(&mut self, mut x: u32) -> u32 {
        while self.parent[x as usize] != x {
            let up = self.parent[self.parent[x as usize] as usize];
            self.parent[x as usize] = up;
            x = up;
        }
        x
    }

    fn union(&mut self, a: u32, b: u32) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            self.parent[hi as usize] = lo;
        }
    }
}

/// Label nonzero pixels of a single-channel image, 8-connectivity.
pub fn connected_components(src: &Image) -> Labeled {
    assert_eq!(src.channels, 1);
    let (w, h) = (src.width, src.height);
    let mut labels = vec![0u32; w * h];
    let mut uf = UnionFind::new();

    // Pass 1: provisional labels, merging with the 4 already-seen neighbors
    // (W, NW, N, NE).
    for y in 0..h {
        for x in 0..w {
            if src.data[y * w + x] == 0 {
                continue;
            }
            let mut neighbor = 0u32;
            let mut check = |nl: u32, uf: &mut UnionFind| {
                if nl != 0 {
                    if neighbor == 0 {
                        neighbor = nl;
                    } else {
                        uf.union(neighbor, nl);
                    }
                }
            };
            if x > 0 {
                check(labels[y * w + x - 1], &mut uf);
            }
            if y > 0 {
                if x > 0 {
                    check(labels[(y - 1) * w + x - 1], &mut uf);
                }
                check(labels[(y - 1) * w + x], &mut uf);
                if x + 1 < w {
                    check(labels[(y - 1) * w + x + 1], &mut uf);
                }
            }
            labels[y * w + x] = if neighbor == 0 { uf.make() } else { neighbor };
        }
    }

    // Pass 2: compress to dense final labels in first-appearance scan order
    // and accumulate stats.
    let mut remap: Vec<u32> = vec![0; uf.parent.len()];
    let mut components: Vec<Component> = Vec::new();
    let mut sums: Vec<(u64, u64)> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let l = labels[y * w + x];
            if l == 0 {
                continue;
            }
            let root = uf.find(l);
            let mut dense = remap[root as usize];
            if dense == 0 {
                components.push(Component {
                    area: 0,
                    x: x as u32,
                    y: y as u32,
                    w: 0,
                    h: 0,
                    cx: 0.0,
                    cy: 0.0,
                });
                sums.push((0, 0));
                dense = components.len() as u32;
                remap[root as usize] = dense;
            }
            let c = &mut components[(dense - 1) as usize];
            c.area += 1;
            c.x = c.x.min(x as u32);
            c.y = c.y.min(y as u32);
            c.w = c.w.max(x as u32 + 1);
            c.h = c.h.max(y as u32 + 1);
            let s = &mut sums[(dense - 1) as usize];
            s.0 += x as u64;
            s.1 += y as u64;
            labels[y * w + x] = dense;
        }
    }
    for (c, s) in components.iter_mut().zip(sums.iter()) {
        c.w -= c.x;
        c.h -= c.y;
        c.cx = s.0 as f64 / c.area as f64;
        c.cy = s.1 as f64 / c.area as f64;
    }
    Labeled {
        labels,
        components,
        width: w,
        height: h,
    }
}
