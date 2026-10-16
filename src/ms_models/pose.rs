//! Windows Hello's legacy IR head pose (`IrFaceOrientation`, sensor-adapter resources 104–107).
//!
//! Port of `tools/winhello_models/lbf/pose.py` (bit-exact against
//! `IrFaceOrientation::ComputeOrientation` FaceProcessorCore 0x18000cfe0 under emulation): the first
//! 27 [`LbfAligner`](super::LbfAligner) points → similarity to a fixed 64×64 template → bilinear warp
//! → DoG(σ2 − σ4) → uniform LBP → 11×11 cell histograms (7139 values) → per angle LDA projection and
//! nearest class centroid. Reads the original `MODEL_FILES_104..107.bin` (layouts in
//! `docs/winhello-models/lbf.md`).
//!
//! Not used as a gate: on this camera it reads +24…26° of yaw on frontal faces (lbf.md). The
//! frontal gate uses the Retina detector's pose instead ([`MsRetinaFace::orientation`](super::MsRetinaFace)).
//! Only the no-prior path of `LDARegressor::Regress` is ported: the prior path needs an estimate
//! of the same tracked face from the last 330 ms, and linux_hello has no tracker.

use anyhow::{bail, Result};
use image::GrayImage;
use std::path::Path;

use super::{FaceOrientation, Reader};

const SIZE: usize = 64;
/// `LbpConfig {10,10,11,11,64,64,1,1,1}`: 11×11 cells of 10×10 at stride 5 from (2, 2)
const CELL: usize = 10;
const CELLS: usize = 11;
const STEP: usize = 5;
const BINS: usize = 59;
const FEATURE_DIM: usize = CELLS * CELLS * BINS;
/// 1/255 as the DLL stores it
const INV255: f32 = f32::from_bits(0x3b80_8081);

/// 64×64 template of the first 27 aligner points (`IrFaceOrientation::Initialize` 0x180022754).
#[rustfmt::skip]
const TEMPLATE_27: [[f32; 2]; 27] = [
    [21.661, 20.715], [42.995, 20.614], [32.072, 32.190], [23.588, 42.566], [42.102, 42.218],
    [13.550, 16.565], [26.701, 15.975], [17.949, 21.326], [21.473, 19.455], [21.588, 22.170],
    [25.673, 21.481], [38.041, 16.014], [51.482, 16.429], [39.577, 21.385], [43.209, 19.343],
    [43.202, 21.957], [46.940, 21.119], [29.572, 21.648], [35.385, 21.644], [27.963, 29.334],
    [36.591, 29.083], [25.790, 33.699], [38.957, 33.061], [32.344, 40.653], [32.416, 42.882],
    [32.316, 43.290], [32.499, 45.906],
];

/// `GaussianFilteringSigma2` kernel (rdata 0x18004b1a0)
#[rustfmt::skip]
const K_SIGMA2: [f32; 13] = [
    0.0022182, 0.0087731, 0.0270232, 0.0648252, 0.1211094, 0.1762131, 0.1996756,
    0.1762131, 0.1211094, 0.0648252, 0.0270232, 0.0087731, 0.0022182,
];

/// `GaussianFilteringSigma4` kernel (rdata 0x18004b1e0)
#[rustfmt::skip]
const K_SIGMA4: [f32; 25] = [
    0.0011099, 0.0022773, 0.0043897, 0.0079487, 0.0135211, 0.0216067, 0.0324355,
    0.0457414, 0.0605975, 0.0754148, 0.0881688, 0.0968345, 0.0999084, 0.0968345,
    0.0881688, 0.0754148, 0.0605975, 0.0457414, 0.0324355, 0.0216067, 0.0135211,
    0.0079487, 0.0043897, 0.0022773, 0.0011099,
];

type Plane = [[f32; SIZE]; SIZE];

/// One angle: LDA (`LoadLDA` 0x180018ad8) and class centroids (`LoadKNN` 0x180018810).
struct LdaRegressor {
    mean: Vec<f32>,
    /// `dim` rows of `FEATURE_DIM`
    basis: Vec<f32>,
    labels: Vec<f32>,
    /// `labels.len()` rows of `dim`
    centroids: Vec<f32>,
    dim: usize,
}

impl LdaRegressor {
    fn load(lda: &Path, knn: &Path) -> Result<Self> {
        let mut r = Reader::open(lda)?;
        let dim = r.u32()? as usize;
        let mean = r.f32s(FEATURE_DIM)?;
        let basis = r.f32s(dim * FEATURE_DIM)?;
        r.finish()?;
        let mut r = Reader::open(knn)?;
        let (k, _gamma, n, m) = (r.u32()?, r.u32()?, r.u32()? as usize, r.u32()? as usize);
        if k != 1 || m != dim {
            bail!("{}: k = {k}, centroid dim {m} (expected k = 1, dim {dim})", knn.display());
        }
        let labels = r.f32s(n)?;
        let centroids = r.f32s(n * m)?;
        r.finish()?;
        Ok(Self { mean, basis, labels, centroids, dim })
    }

    /// `LDARegressor::compute` 0x18000ba00 without a prior: label of the nearest centroid
    /// (Euclidean, first minimum; 0x18000b498).
    fn angle(&self, feature: &[f32]) -> f32 {
        let centred: Vec<f64> = feature.iter().zip(&self.mean).map(|(f, m)| (f - m) as f64).collect();
        let projected: Vec<f32> = self
            .basis
            .chunks(FEATURE_DIM)
            .map(|row| row.iter().zip(&centred).map(|(b, c)| *b as f64 * c).sum::<f64>() as f32)
            .collect();
        let distance = |c: &[f32]| c.iter().zip(&projected).map(|(c, p)| (c - p) * (c - p)).sum::<f32>();
        let mut best = (f32::INFINITY, 0);
        for (i, c) in self.centroids.chunks(self.dim).enumerate() {
            let d = distance(c);
            if d < best.0 {
                best = (d, i);
            }
        }
        self.labels[best.1]
    }
}

/// `IrFaceOrientation` with its yaw (106/107) and pitch (104/105) regressors.
pub struct IrFaceOrientation {
    yaw: LdaRegressor,
    pitch: LdaRegressor,
}

impl IrFaceOrientation {
    /// Load from the directory with `MODEL_FILES_104.bin` … `MODEL_FILES_107.bin`
    /// (`CreateIrFaceOrientation` 0x180043278).
    pub fn load(dir: impl AsRef<Path>) -> Result<Self> {
        let file = |n: u32| dir.as_ref().join(format!("MODEL_FILES_{n}.bin"));
        Ok(Self {
            yaw: LdaRegressor::load(&file(107), &file(106))?,
            pitch: LdaRegressor::load(&file(105), &file(104))?,
        })
    }

    /// Head pose from the aligner's points on the 8-bit lit frame (`ComputeOrientation`
    /// 0x18000cfe0, no prior).
    pub fn estimate(&self, image: &GrayImage, points: &[[f32; 2]]) -> Result<FaceOrientation> {
        let (a, b, tx, ty) = similarity_to_template(points)?;
        let face = warp_face(image, a, b, tx, ty);
        let (g2, g4) = (gaussian(&face, &K_SIGMA2), gaussian(&face, &K_SIGMA4));
        let mut dog = [[0.0f32; SIZE]; SIZE];
        for ((d, x), y) in dog.iter_mut().flatten().zip(g2.iter().flatten()).zip(g4.iter().flatten()) {
            *d = x - y;
        }
        let feature = lbp_feature(&lbp_codes(&dog));
        let mut pose = FaceOrientation::new(self.pitch.angle(&feature), self.yaw.angle(&feature), roll(a, b));
        if pose.is_frontal() {
            frontal_from_points(points, &mut pose);
        }
        Ok(pose)
    }
}

/// Least-squares similarity from the first ≤ 27 points to the template
/// (`CalculateSimilarityTransform` 0x18000d4c8): template ≈ [a b; −b a]·p + t.
fn similarity_to_template(points: &[[f32; 2]]) -> Result<(f32, f32, f32, f32)> {
    let (mut sx, mut sy, mut tx_, mut ty_) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let (mut sxx, mut syy, mut xs, mut sxty, mut txsy, mut syty) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let n = points.len().min(TEMPLATE_27.len());
    for (&[x, y], &[u, v]) in points.iter().zip(&TEMPLATE_27) {
        sx += x;
        sy += y;
        ty_ += v;
        tx_ += u;
        sxx += x * x;
        syy += y * y;
        xs += u * x;
        sxty += x * v;
        txsy += u * y;
        syty += y * v;
    }
    let s2 = syy + sxx;
    let n = n as f32;
    let den = (sx * sx + sy * sy) - n * s2;
    if den.abs() < 1e-6 {
        bail!("degenerate landmarks");
    }
    let dot = syty + xs;
    let cross = txsy - sxty;
    let a = ((tx_ * sx + ty_ * sy) - dot * n) / den;
    let b = ((tx_ * sy - ty_ * sx) - cross * n) / den;
    let tx = ((cross * sy + dot * sx) - s2 * tx_) / den;
    let ty = ((dot * sy - cross * sx) - s2 * ty_) / den;
    Ok((a, b, tx, ty))
}

/// 64×64 face × 1/255 (`ApplySimilarityTransformToImage` 0x18000c010 with
/// `BilinearInterpolationWithPadding` 0x18000c6b0: outside pixels read 0).
fn warp_face(image: &GrayImage, a: f32, b: f32, tx: f32, ty: f32) -> Plane {
    let (w, h) = (image.width() as i64, image.height() as i64);
    let data = image.as_raw();
    let n2 = a * a + b * b;
    let c13 = a / n2;
    let c14 = -b / n2;
    let c15 = (-tx * b - ty * a) / n2;
    let c16 = (ty * b - tx * a) / n2;
    let px = |x: i64, y: i64| {
        if (0..w).contains(&x) && (0..h).contains(&y) {
            data[(y * w + x) as usize] as f32
        } else {
            0.0
        }
    };
    let mut out = [[0.0f32; SIZE]; SIZE];
    for (row, line) in out.iter_mut().enumerate() {
        for (col, v) in line.iter_mut().enumerate() {
            let (c, r) = (col as f32, row as f32);
            let x = (c * c13 + r * c14) + c16;
            let y = (r * c13 - c * c14) + c15;
            let (x0, y0) = (x.floor(), y.floor());
            let (fx, fy) = (x - x0, y - y0);
            let (x0, y0) = (x0 as i64, y0 as i64);
            let top = (px(x0 + 1, y0) - px(x0, y0)) * fx + px(x0, y0);
            let bottom = (px(x0 + 1, y0 + 1) - px(x0, y0 + 1)) * fx + px(x0, y0 + 1);
            *v = ((bottom - top) * fy + top) * INV255;
        }
    }
    out
}

/// Index into a row/column with mirror padding that repeats the edge sample.
fn mirror(i: isize) -> usize {
    let n = SIZE as isize;
    (if i < 0 {
        -i - 1
    } else if i >= n {
        2 * n - 1 - i
    } else {
        i
    }) as usize
}

/// `GaussianFilteringSigma2/4` as the x64 code executes it: separable, and the horizontal pass only
/// computes columns 4g and 4g+1, writing copies of them into 4g+2 and 4g+3 (lbf.md, L6).
fn gaussian(input: &Plane, kernel: &[f32]) -> Plane {
    let r = (kernel.len() / 2) as isize;
    let mut horizontal = [[0.0f32; SIZE]; SIZE];
    for (out, row) in horizontal.iter_mut().zip(input) {
        for (col, v) in out.iter_mut().enumerate() {
            let col = (col & !2) as isize;
            *v = kernel.iter().enumerate().fold(0.0, |s, (k, w)| s + w * row[mirror(col + k as isize - r)]);
        }
    }
    let mut out = [[0.0f32; SIZE]; SIZE];
    for (row, line) in out.iter_mut().enumerate() {
        for (col, v) in line.iter_mut().enumerate() {
            *v = kernel
                .iter()
                .enumerate()
                .fold(0.0, |s, (k, w)| s + w * horizontal[mirror(row as isize + k as isize - r)][col]);
        }
    }
    out
}

/// The standard 59-bin uniform LBP mapping; `lbpLookUpTable` (0x18004b0a0) is byte for byte this
/// table.
fn uniform_lbp_lut() -> [u8; 256] {
    let mut lut = [58u8; 256];
    let mut next = 0;
    for (code, v) in lut.iter_mut().enumerate() {
        let code = code as u8;
        if (code ^ code.rotate_right(1)).count_ones() <= 2 {
            *v = next;
            next += 1;
        }
    }
    lut
}

/// `EncodeLBPFast` 0x18000ca00: bit set when the neighbour is greater than the centre, bit order
/// TL, T, TR, R, BR, B, BL, L; only x, y ∈ [2, 62) are coded.
fn lbp_codes(dog: &Plane) -> [[u8; SIZE]; SIZE] {
    const NEIGHBOURS: [(isize, isize); 8] = [(-1, -1), (-1, 0), (-1, 1), (0, 1), (1, 1), (1, 0), (1, -1), (0, -1)];
    let lut = uniform_lbp_lut();
    let mut out = [[0u8; SIZE]; SIZE];
    for y in 2..SIZE - 2 {
        for x in 2..SIZE - 2 {
            let centre = dog[y][x];
            let code = NEIGHBOURS.iter().enumerate().fold(0usize, |code, (bit, &(dy, dx))| {
                let n = dog[(y as isize + dy) as usize][(x as isize + dx) as usize];
                code | (usize::from(centre < n) << bit)
            });
            out[y][x] = lut[code];
        }
    }
    out
}

/// `ComputeOrientationLBPFeature` 0x18000ce90, × 0.01.
fn lbp_feature(codes: &[[u8; SIZE]; SIZE]) -> Vec<f32> {
    let mut counts = vec![0u32; FEATURE_DIM];
    for cy in 0..CELLS {
        for cx in 0..CELLS {
            let hist = &mut counts[(cy * CELLS + cx) * BINS..][..BINS];
            for row in &codes[2 + STEP * cy..][..CELL] {
                for &code in &row[2 + STEP * cx..][..CELL] {
                    hist[code as usize] += 1;
                }
            }
        }
    }
    counts.into_iter().map(|c| c as f32 * 0.01).collect()
}

/// The third FaceOrientationResult value: 2·acos(w) of the quaternion of the rows (a, −b, 0),
/// (b, a, 0), (0, 0, 1) (`XMQuaternionRotationMatrix` 0x18000c87c), in degrees. Unsigned and shrunk
/// by the image→template scale, so not a true roll angle (lbf.md).
fn roll(a: f32, b: f32) -> f32 {
    let (a, b) = (a as f64, b as f64);
    let w = if 2.0 * a > 0.0 {
        (1.0 + a) / (b * b + (1.0 + a).powi(2)).sqrt()
    } else {
        -b / ((1.0 - a).powi(2) + b * b).sqrt()
    };
    (2.0 * w.clamp(-1.0, 1.0).acos()) as f32 * 57.295_776_4
}

/// Whether `p` is inside the quad (same sign convention as the SSE code).
fn inside(quad: &[[f32; 2]; 4], p: [f32; 2]) -> bool {
    (0..4).all(|i| {
        let e0 = [quad[i][0] - p[0], quad[i][1] - p[1]];
        let q = quad[(i + 1) & 3];
        let e1 = [q[0] - p[0], q[1] - p[1]];
        e1[1] * e0[0] - e1[0] * e0[1] >= 0.0
    })
}

/// `EstimateFaceFrontalFromAlignmentPoints` 0x18000b640, run when the angles set no flag: the nose
/// (p2) must stay inside the eye/mouth quad (p0, p1, p4, p3) shrunk toward the opposite corners by
/// the margin t (bisection, 5 steps); t < 0.125 sets the flag of the quad edge closest to the nose
/// (eyes → pitch > 25, right side → yaw < −20, mouth → pitch < −25, left side → yaw > 20).
fn frontal_from_points(points: &[[f32; 2]], pose: &mut FaceOrientation) {
    let base = [points[0], points[1], points[4], points[3]];
    let nose = points[2];
    let shrink = |t: f32| {
        std::array::from_fn::<_, 4, _>(|i| {
            let (o, p) = (base[(i + 2) & 3], base[i]);
            [t * o[0] + (1.0 - t) * p[0], t * o[1] + (1.0 - t) * p[1]]
        })
    };
    let (mut t, mut quad) = (0.0f32, base);
    if inside(&base, nose) {
        let (mut lo, mut hi) = (0.0f32, 0.5f32);
        t = 0.25;
        for _ in 0..5 {
            quad = shrink(t);
            if inside(&quad, nose) {
                lo = t;
            } else {
                hi = t;
            }
            t = (lo + hi) * 0.5;
        }
    }
    pose.frontal_margin = t;
    if t < 0.125 {
        let distance = |i: usize| {
            let (q, r) = (quad[i], quad[(i + 1) & 3]);
            let e = [r[0] - q[0], r[1] - q[1]];
            let v = [nose[0] - q[0], nose[1] - q[1]];
            let k = (v[0] * e[0] + v[1] * e[1]) / (e[0] * e[0] + e[1] * e[1]);
            (v[0] - k * e[0]).hypot(v[1] - k * e[1])
        };
        let edge = (0..4).min_by(|&i, &j| distance(i).total_cmp(&distance(j))).expect("4 edges");
        pose.flags[[2, 1, 3, 0][edge]] = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_lut_has_58_uniform_codes() {
        let lut = uniform_lbp_lut();
        assert_eq!(lut.iter().filter(|&&v| v < 58).count(), 58);
        assert_eq!(lut[255], 57);
    }

    #[test]
    fn template_maps_to_itself() {
        let (a, b, tx, ty) = similarity_to_template(&TEMPLATE_27).unwrap();
        assert!((a - 1.0).abs() < 1e-3 && b.abs() < 1e-3 && tx.abs() < 1e-2 && ty.abs() < 1e-2);
    }
}
