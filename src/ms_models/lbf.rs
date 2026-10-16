//! Windows Hello's legacy IR landmark aligner (`LbfAligner`, sensor-adapter resource MODEL_FILES 102).
//!
//! Port of `tools/winhello_models/lbf/aligner.py`, which is bit-exact against
//! `LbfAligner::Aligner::Predict` (FaceProcessorCore 0x180010cb0) run under emulation. It reads the
//! original `.cefa` database (byte layout in `docs/winhello-models/lbf.md`) and gives Windows' 31
//! points (slots 0–4 = eye centres, nose tip, mouth corners; F58) plus the face/non-face classifier
//! score. Arithmetic is float32 in the same order as the x64 code.

use anyhow::{bail, Result};
use image::GrayImage;
use std::path::Path;

use super::Reader;

/// A 2×3 similarity/affine matrix, rows `[a, b, tx]`, `[c, d, ty]`.
type Affine = [[f32; 3]; 2];

/// One tree node (24 bytes in the file).
#[derive(Clone, Copy)]
struct Node {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    threshold: f32,
    /// Low 16 bits: child when `threshold >= feature`, high 16 bits otherwise; bit 15 = leaf flag,
    /// bits 0–14 = node index or leaf row.
    children: u32,
}

/// Linear leaf regressor: the rows of the reached leaves plus the last (bias) row are summed
/// (`Regressor::Predict` 0x1800114c0).
struct Regressor {
    rows: usize,
    cols: usize,
    /// `int16 × scale` when stored compressed
    weights: Vec<f32>,
}

impl Regressor {
    fn read(r: &mut Reader) -> Result<Self> {
        let compressed = r.u8()? != 0;
        let rows = r.u32()? as usize;
        let cols = r.u32()? as usize;
        let weights = if compressed {
            let scale = r.f32()?;
            (0..rows * cols).map(|_| Ok(r.i16()? as f32 * scale)).collect::<Result<_>>()?
        } else {
            r.f32s(rows * cols)?
        };
        Ok(Self { rows, cols, weights })
    }

    fn predict(&self, leaves: &[usize]) -> Vec<f32> {
        let mut out = vec![0.0f32; self.cols];
        for &row in leaves.iter().chain([self.rows - 1].iter()) {
            for (o, w) in out.iter_mut().zip(&self.weights[row * self.cols..(row + 1) * self.cols]) {
                *o += *w;
            }
        }
        out
    }
}

struct Stage {
    /// Tree `t` works on landmark `t / trees_per_point`
    trees_per_point: usize,
    trees: Vec<Vec<Node>>,
    regressor: Regressor,
}

/// Result of [`LbfAligner::align`].
#[derive(Debug, Clone)]
pub struct Alignment {
    /// 31 points in image pixels
    pub points: Vec<[f32; 2]>,
    /// Classifier score; Windows rejects the face when it is ≤ 0 (`AlignerContext`+260)
    pub score: f32,
}

impl Alignment {
    pub fn accepted(&self) -> bool {
        self.score > 0.0
    }
}

pub struct LbfAligner {
    mean: Vec<[f32; 2]>,
    stages: Vec<Stage>,
    classifier: Regressor,
}

/// The unit square that the detector rectangle is fitted to (Aligner+96, set in `LoadDatabase`).
const UNIT_BOX: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]];

impl LbfAligner {
    /// Read `MODEL_FILES_102.cefa` (`Aligner::LoadDatabase` 0x180017d80).
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let mut r = Reader::open(path.as_ref())?;
        let magic = r.u16()?;
        let version = r.u8()?;
        if magic != 0xFACE || version != 10 {
            bail!("{}: not an LbfAligner database (magic {magic:#x}, version {version})", path.as_ref().display());
        }
        r.skip(7)?;
        let num_points = r.u32()? as usize;
        let num_stages = r.u32()?;
        let mean = r.f32s(2 * num_points)?.chunks(2).map(|p| [p[0], p[1]]).collect();
        let mut stages = Vec::new();
        for _ in 0..num_stages {
            let num_trees = r.u32()? as usize;
            let trees_per_point = r.u32()? as usize;
            if trees_per_point == 0 {
                bail!("trees per point is 0");
            }
            let mut trees = Vec::with_capacity(num_trees);
            for _ in 0..num_trees {
                let n = r.u32()? as usize;
                let nodes = (0..n)
                    .map(|_| {
                        Ok(Node {
                            x1: r.f32()?,
                            y1: r.f32()?,
                            x2: r.f32()?,
                            y2: r.f32()?,
                            threshold: r.f32()?,
                            children: r.u32()?,
                        })
                    })
                    .collect::<Result<_>>()?;
                trees.push(nodes);
            }
            stages.push(Stage { trees_per_point, trees, regressor: Regressor::read(&mut r)? });
        }
        if r.u32()? != 1 {
            bail!("missing classifier");
        }
        let classifier = Regressor::read(&mut r)?;
        r.finish()?;
        Ok(Self { mean, stages, classifier })
    }

    /// Align the face in the detector rectangle `[left, top, right, bottom]` (right/bottom
    /// exclusive) on the 8-bit lit frame: detection mode of `Aligner::Predict`.
    pub fn align(&self, image: &GrayImage, rect: [i32; 4]) -> Alignment {
        let [l, t, r, b] = rect.map(|v| v as f32);
        let corners = [[l, t], [r - 1.0, t], [l, b - 1.0], [r - 1.0, b - 1.0]];
        let mut shape = affine(&similarity(&UNIT_BOX, &corners), &self.mean);
        for stage in &self.stages {
            let m = diff_similarity(&self.mean, &shape);
            let leaves = stage_leaves(stage, &m, &shape, image);
            let delta = stage.regressor.predict(&leaves);
            for (p, d) in shape.iter_mut().zip(delta.chunks(2)) {
                *p = [(m[0][1] * d[1] + m[0][0] * d[0]) + p[0], (m[1][1] * d[1] + m[1][0] * d[0]) + p[1]];
            }
        }
        // Classifier::Predict 0x18001073c: the last stage's trees on the final shape.
        let last = self.stages.last().expect("at least one stage");
        let leaves = stage_leaves(last, &diff_similarity(&self.mean, &shape), &shape, image);
        let score = self.classifier.predict(&leaves)[0];
        Alignment { points: shape, score }
    }
}

/// The reached leaf row of every tree (`Stage::Predict` 0x180010850). Node offsets are in
/// mean-shape units around the tree's landmark, rotated and scaled by `m`; pixels outside the
/// image read 0; feature = (I1 − I2) / (I1 + I2 + FLT_MIN).
fn stage_leaves(stage: &Stage, m: &Affine, shape: &[[f32; 2]], image: &GrayImage) -> Vec<usize> {
    let (w, h) = (image.width() as i64, image.height() as i64);
    let data = image.as_raw();
    let [c0, c1, c3, c4] = [m[0][0], m[0][1], m[1][0], m[1][1]];
    stage
        .trees
        .iter()
        .enumerate()
        .map(|(t, nodes)| {
            let [lx, ly] = shape[t / stage.trees_per_point];
            let pixel = |dx: f32, dy: f32| {
                let px = (((c0 * dx + lx) + c1 * dy) + 0.5) as i64;
                let py = (((c3 * dx + ly) + c4 * dy) + 0.5) as i64;
                if (0..w).contains(&px) && (0..h).contains(&py) {
                    data[(py * w + px) as usize] as f32
                } else {
                    0.0
                }
            };
            let mut node = &nodes[0];
            loop {
                let (p1, p2) = (pixel(node.x1, node.y1), pixel(node.x2, node.y2));
                let feature = (p1 - p2) / ((p2 + p1) + f32::MIN_POSITIVE);
                let child = if node.threshold >= feature { node.children & 0xFFFF } else { node.children >> 16 };
                let index = (child & 0x7FFF) as usize;
                if child & 0x8000 != 0 {
                    return index;
                }
                node = &nodes[index];
            }
        })
        .collect()
}

/// Least-squares similarity `src → dst` (`ComputeSimilarityTransformCoefficient` 0x180011c10):
/// `x' = a·x + b·y + tx`, `y' = −b·x + a·y + ty`.
fn similarity(src: &[[f32; 2]], dst: &[[f32; 2]]) -> Affine {
    let (mut sx, mut sy, mut dx, mut dy) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let (mut norm, mut dot, mut cross) = (0.0f32, 0.0f32, 0.0f32);
    for (&[x, y], &[u, v]) in src.iter().zip(dst) {
        sy += y;
        sx += x;
        dx += u;
        dy += v;
        norm += y * y + x * x;
        dot += v * y + u * x;
        cross += u * y - v * x;
    }
    let n = src.len() as f32;
    let (my, mx) = (sy / n, sx / n);
    let (norm, cross) = (norm / n, cross / n);
    let (mu, mv, dot) = (dx / n, dy / n, dot / n);
    let var = (norm - mx * mx) - my * my;
    let c31 = -mx / var;
    let c32 = norm / var;
    let c33 = 1.0 / var;
    let c34 = -my / var;
    let a = (c31 * mu + c33 * dot) + c34 * mv;
    let b = (c34 * mu + c33 * cross) - c31 * mv;
    let ty = (c34 * dot - c31 * cross) + c32 * mv;
    let tx = (c34 * cross + c31 * dot) + c32 * mu;
    [[a, b, tx], [-b, a, ty]]
}

/// `AffineTransform` 0x180011b8c.
fn affine(m: &Affine, points: &[[f32; 2]]) -> Vec<[f32; 2]> {
    points
        .iter()
        .map(|&[x, y]| [(m[0][0] * x + m[0][1] * y) + m[0][2], (m[1][0] * x + m[1][1] * y) + m[1][2]])
        .collect()
}

/// The transform from mean-shape coordinates onto `shape`
/// (`ComputeDifferencialSimilarityTransformationCoefficient` 0x180011534): the rotation of a
/// similarity fit between the centred mean shape and the centred, per-axis normalised shape, then
/// separate x and y scales and a translation.
fn diff_similarity(mean: &[[f32; 2]], shape: &[[f32; 2]]) -> Affine {
    let n = mean.len() as f32;
    let centre = |p: &[[f32; 2]]| {
        let (mut sx, mut sy) = (0.0f32, 0.0f32);
        for &[x, y] in p {
            sy += y;
            sx += x;
        }
        let (cx, cy) = (sx / n, sy / n);
        (cx, cy, p.iter().map(|&[x, y]| [x - cx, y - cy]).collect::<Vec<_>>())
    };
    let (mx, my, a) = centre(mean);
    let (cx, cy, mut s) = centre(shape);
    let (mut ax2, mut ay2, mut sx2, mut sy2) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for (&[ax, ay], &[sx, sy]) in a.iter().zip(&s) {
        ay2 += ay * ay;
        ax2 += ax * ax;
        sx2 += sx * sx;
        sy2 += sy * sy;
    }
    let (nsx, nsy) = (sx2.sqrt(), sy2.sqrt());
    let (kx, ky) = (ax2.sqrt() / nsx, ay2.sqrt() / nsy);
    for p in &mut s {
        p[0] *= kx;
        p[1] *= ky;
    }
    let mut m = similarity(&a, &s);
    let inv = 1.0 / (m[0][0] * m[0][0] + m[0][1] * m[0][1]).sqrt();
    for row in &mut m {
        row[0] *= inv;
        row[1] *= inv;
        row[2] = 0.0;
    }
    let (mut rx2, mut ry2) = (0.0f32, 0.0f32);
    for [x, y] in affine(&m, &a) {
        rx2 += x * x;
        ry2 += y * y;
    }
    let (kx, ky) = (nsx / rx2.sqrt(), nsy / ry2.sqrt());
    m[0][0] *= kx;
    m[0][1] *= kx;
    m[1][0] *= ky;
    m[1][1] *= ky;
    m[1][2] = cy - (m[1][1] * my + m[1][0] * mx);
    m[0][2] = cx - (m[0][1] * my + m[0][0] * mx);
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_recovers_a_known_transform() {
        let src = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]];
        let dst = src.map(|[x, y]| [2.0 * x + 0.5 * y + 10.0, -0.5 * x + 2.0 * y + 20.0]);
        let m = similarity(&src, &dst);
        for (got, want) in m.iter().flatten().zip([2.0, 0.5, 10.0, -0.5, 2.0, 20.0]) {
            assert!((got - want).abs() < 1e-4, "{m:?}");
        }
    }
}
