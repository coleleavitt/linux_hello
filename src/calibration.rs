//! Similarity → estimated false-accept rate through a per-camera calibration table, as Windows'
//! distance → GFP curve (F27, F51, F60). Windows does not threshold the distance itself: each
//! recognition model carries (FPR, TAR, distance) tables per camera hardware model and a face is
//! recognised when the estimated FAR is below [`TARGET_FAR`]. `models/calibration.json` holds the
//! same for ArcFace, in cosine similarity, keyed by the IR camera's USB `vid:pid`
//! (docs/calibration.md); [`load_windows`] reads Microsoft's own table for its recogniser.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tracing::info;

use crate::authenticator::MAX_PAIRS;

/// A face is recognised when its estimated FAR is below this (F27).
pub const TARGET_FAR: f64 = 1e-5;

/// One table row: the false-accept and true-accept rates of one comparison at a similarity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Row {
    pub fpr: f64,
    pub tar: f64,
    pub similarity: f32,
}

/// Rows by increasing similarity (decreasing FPR).
#[derive(Debug, Clone)]
pub struct CalibrationTable {
    name: String,
    rows: Vec<Row>,
}

impl CalibrationTable {
    /// `rows` as (FPR, TAR, similarity), in any order; FPR must fall as the similarity rises.
    pub fn new(name: &str, rows: impl IntoIterator<Item = (f64, f64, f32)>) -> Result<Self> {
        let mut rows: Vec<Row> = rows.into_iter().map(|(fpr, tar, similarity)| Row { fpr, tar, similarity }).collect();
        rows.sort_by(|a, b| a.similarity.total_cmp(&b.similarity));
        if rows.len() < 2 {
            bail!("calibration table {name}: at least two rows are needed");
        }
        let ordered = rows.windows(2).all(|w| w[0].similarity < w[1].similarity && w[0].fpr > w[1].fpr);
        let in_range = rows.iter().all(|r| r.fpr > 0.0 && r.fpr <= 1.0 && r.similarity.abs() < 1.0);
        if !ordered || !in_range {
            bail!("calibration table {name}: FPR must fall strictly as the similarity rises, within (0, 1]");
        }
        Ok(Self { name: name.to_string(), rows })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Estimated FAR of one print comparison at `similarity` (`sub_1800397B4`, F51), with
    /// distance = 1 − similarity: above the last row, `FPR₀ · exp((d − d₀) / |d₀|)`; otherwise
    /// linear interpolation of the FPR between the bracketing rows, extrapolated below the first.
    pub fn pair_far(&self, similarity: f32) -> f64 {
        let top = self.rows[self.rows.len() - 1];
        if similarity >= top.similarity {
            let d0 = 1.0 - f64::from(top.similarity);
            return top.fpr * (f64::from(top.similarity - similarity) / d0.abs()).exp();
        }
        let i = self.rows.iter().position(|r| r.similarity > similarity).expect("below the last row").max(1);
        let (a, b) = (self.rows[i - 1], self.rows[i]);
        a.fpr + f64::from(similarity - a.similarity) * (b.fpr - a.fpr) / f64::from(b.similarity - a.similarity)
    }

    /// Estimated FAR of an attempt whose best comparison, against a template of `prints` prints,
    /// scored `similarity`: [`pair_far`](Self::pair_far) × the comparisons the attempt can make,
    /// `prints` × [`MAX_PAIRS`] (the union bound of docs/calibration.md), at most 1. Windows scales
    /// its GFP by the number of comparisons too (F51, F52). Compared with [`TARGET_FAR`].
    pub fn far(&self, similarity: f32, prints: usize) -> f64 {
        (self.pair_far(similarity) * (prints * MAX_PAIRS as usize) as f64).min(1.0)
    }
}

#[derive(Deserialize)]
struct File {
    default: String,
    #[serde(default)]
    cameras: HashMap<String, String>,
    tables: HashMap<String, Table>,
}

#[derive(Deserialize)]
struct Table {
    rows: Vec<(f64, f64, f32)>,
}

/// The table of `path` (`models/calibration.json`) for the IR camera `camera` (USB `vid:pid`,
/// [`crate::sysfs::usb_id`]), or the file's default table when the camera is unknown or not listed.
pub fn load(path: impl AsRef<Path>, camera: Option<&str>) -> Result<CalibrationTable> {
    let path = path.as_ref();
    let file: File = serde_json::from_slice(&fs::read(path).with_context(|| format!("reading {}", path.display()))?)
        .with_context(|| format!("parsing {}", path.display()))?;
    let listed = camera.and_then(|c| file.cameras.get(c));
    let name = listed.unwrap_or(&file.default);
    let table = file.tables.get(name).with_context(|| format!("{}: no table named {name}", path.display()))?;
    info!(camera, table = name, listed = listed.is_some(), "face-match calibration table");
    CalibrationTable::new(name, table.rows.iter().copied())
}

#[derive(Deserialize)]
struct WindowsFile {
    calibration: Vec<WindowsTable>,
}

/// One (FPR, TAR, distance) table of a `.cefa` header (docs/winhello-models/cefa.md §2.2).
#[derive(Deserialize)]
struct WindowsTable {
    hardware_model: u32,
    rows_fpr_tar_distance: Vec<(f64, f64, f32)>,
}

/// Microsoft's distance → FAR table of a recognition model (F51), from the JSON that
/// `tools/winhello_models/cefa unpack` writes next to the model, as a table in similarity =
/// 1 − distance. The first hardware model's table is taken: the five shipped tables are identical,
/// and which camera classes the ids mean is not known (cefa.md §7).
pub fn load_windows(path: impl AsRef<Path>) -> Result<CalibrationTable> {
    let path = path.as_ref();
    let file: WindowsFile =
        serde_json::from_slice(&fs::read(path).with_context(|| format!("reading {}", path.display()))?)
            .with_context(|| format!("parsing {}", path.display()))?;
    let table = file.calibration.first().with_context(|| format!("{}: no calibration table", path.display()))?;
    let rows = table.rows_fpr_tar_distance.iter().map(|&(fpr, tar, distance)| (fpr, tar, 1.0 - distance));
    CalibrationTable::new(&format!("windows_v5/{}", table.hardware_model), rows)
}

/// The shipped table, for tests of the decision rules.
#[cfg(test)]
pub(crate) fn lfw_gray() -> CalibrationTable {
    load(concat!(env!("CARGO_MANIFEST_DIR"), "/models/calibration.json"), Some("30c9:00f3")).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, rel: f64) -> bool {
        (a - b).abs() <= rel * b
    }

    #[test]
    fn interpolates_linearly_and_extrapolates_like_windows() {
        let t = CalibrationTable::new("t", [(1e-4, 0.99, 0.4), (1e-2, 0.9, 0.2), (1e-3, 0.95, 0.3)]).unwrap();
        // On a row, between rows, below the first row (linear in the FPR).
        assert_eq!(t.pair_far(0.3), 1e-3);
        assert!(close(t.pair_far(0.35), 5.5e-4, 1e-6));
        assert!(close(t.pair_far(0.25), 5.5e-3, 1e-6));
        assert!(close(t.pair_far(0.1), 1.9e-2, 1e-6));
        // Above the last row: FPR₀ · exp((d − d₀) / |d₀|) with d = 1 − similarity, d₀ = 0.6.
        assert_eq!(t.pair_far(0.4), 1e-4);
        assert!(close(t.pair_far(0.7), 1e-4 * (-0.5f64).exp(), 1e-6));
        assert!(close(t.pair_far(1.0), 1e-4 * (-1.0f64).exp(), 1e-6));
        // 15 prints × 16 pairs = 240 comparisons; 16 prints × 16 = 256.
        assert_eq!(t.far(0.3, 15), 1e-3 * 240.0);
        assert_eq!(t.far(0.3, 16), 1e-3 * 256.0);
        assert_eq!(t.far(0.0, 15), 1.0);
    }

    #[test]
    fn reproduces_the_windows_v5_curve() {
        // First rows of the v5 table (docs/winhello-models/cefa.md), distance → similarity.
        let v5 = [(2.03519e-06, 0.995666, 0.305), (4.12243e-06, 0.996797, 0.325), (6.12639e-06, 0.997209, 0.336)]
            .into_iter()
            .chain([(8.2865e-06, 0.997510, 0.345), (1e-05, 0.997664, 0.350), (3.08245e-05, 0.998230, 0.381)])
            .map(|(fpr, tar, d): (f64, f64, f32)| (fpr, tar, 1.0 - d));
        let t = CalibrationTable::new("v5", v5).unwrap();
        assert_eq!(t.pair_far(0.65), 1e-5, "FAR 1e-5 ↔ distance 0.350");
        assert!(close(t.pair_far(0.6475), 1e-5 + 0.0025 / 0.031 * (3.08245e-05 - 1e-5), 1e-6));
        assert!(close(t.pair_far(0.8), 2.03519e-06 * ((0.2 - 0.305f64) / 0.305).exp(), 1e-5));
    }

    #[test]
    fn reads_the_windows_container_header() {
        let dir = std::env::temp_dir().join(format!("linux_hello_windows_table_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("MODEL_FILES_123.json");
        fs::write(
            &path,
            r#"{"tech": 11, "calibration": [
              {"hardware_model": 0, "ids": [], "rows_fpr_tar_distance": [[2.03519e-06, 0.995666, 0.305], [1e-05, 0.997664, 0.35]]},
              {"hardware_model": 1, "ids": [], "rows_fpr_tar_distance": [[2.03519e-06, 0.995666, 0.305], [1e-05, 0.997664, 0.35]]}]}"#,
        )
        .unwrap();
        let t = load_windows(&path).unwrap();
        assert_eq!(t.name(), "windows_v5/0");
        assert_eq!(t.rows().len(), 2);
        assert!(close(t.pair_far(0.65), 1e-5, 1e-6), "FAR 1e-5 ↔ distance 0.350");
        assert!(t.pair_far(0.66) < 1e-5 && t.pair_far(0.64) > 1e-5);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_unordered_tables() {
        assert!(CalibrationTable::new("one", [(1e-3, 0.9, 0.3)]).is_err());
        assert!(CalibrationTable::new("flat", [(1e-3, 0.9, 0.3), (1e-3, 0.9, 0.4)]).is_err());
        assert!(CalibrationTable::new("rising", [(1e-4, 0.9, 0.3), (1e-3, 0.9, 0.4)]).is_err());
        assert!(CalibrationTable::new("same", [(1e-3, 0.9, 0.3), (1e-4, 0.9, 0.3)]).is_err());
    }

    #[test]
    fn shipped_table_keeps_the_calibrated_operating_point() {
        let t = lfw_gray();
        assert_eq!(t.name(), "lfw_gray");
        // The 0.50 row is the per-attempt bound for 15 prints: FAR 1e-5 (to the precision of the
        // JSON float). A 16-print template needs 0.503, a 31-print one (16 enrolled + 10 augmented
        // + 5 on-failure) 0.525; a single print 0.42.
        assert!(close(t.far(0.5, 15), TARGET_FAR, 1e-12));
        assert!(t.far(0.4999, 15) > TARGET_FAR && t.far(0.5001, 15) < TARGET_FAR);
        assert!(t.far(0.5, 16) > TARGET_FAR && t.far(0.503, 16) < TARGET_FAR);
        assert!(t.far(0.52, 31) > TARGET_FAR && t.far(0.525, 31) < TARGET_FAR);
        assert!(t.far(0.42, 1) > TARGET_FAR && t.far(0.43, 1) < TARGET_FAR);
        // The measured FAR points of docs/calibration.md (1e-5 at 0.346, 1e-6 at 0.401) within 10%.
        assert!(close(t.pair_far(0.346), 1e-5, 0.1));
        assert!(close(t.pair_far(0.4007), 1e-6, 0.1));
        let sweep: Vec<f64> = (-100..=100).map(|k| t.pair_far(k as f32 / 100.0)).collect();
        assert!(sweep.windows(2).all(|w| w[0] > w[1]), "FAR falls over the whole similarity range");
        // An unlisted camera gets the default table.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/models/calibration.json");
        assert_eq!(load(path, Some("ffff:0000")).unwrap().name(), "lfw_gray");
        assert_eq!(load(path, None).unwrap().rows().len(), t.rows().len());
    }
}
