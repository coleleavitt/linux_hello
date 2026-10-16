//! Labelled recordings for the liveness bench (docs/liveness-bench.md): what `linux_hello capture`
//! writes next to the frames and `examples/liveness_bench` reads back.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Who or what was in front of the camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Label {
    /// The enrolled user, live
    Genuine,
    /// A printed photo of the user
    Print,
    /// The user's face on a phone, tablet or monitor
    Screen,
    /// Another person, live
    Impostor,
    /// A mask or a covered face
    Mask,
}

impl Label {
    pub const ALL: [Label; 5] = [Label::Genuine, Label::Print, Label::Screen, Label::Impostor, Label::Mask];

    pub fn as_str(self) -> &'static str {
        match self {
            Label::Genuine => "genuine",
            Label::Print => "print",
            Label::Screen => "screen",
            Label::Impostor => "impostor",
            Label::Mask => "mask",
        }
    }

    /// Whether a recording with this label should be accepted.
    pub fn is_genuine(self) -> bool {
        self == Label::Genuine
    }
}

impl std::str::FromStr for Label {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Label::ALL
            .into_iter()
            .find(|l| l.as_str() == s)
            .ok_or_else(|| format!("unknown label {s}; one of {}", Label::ALL.map(Label::as_str).join(", ")))
    }
}

/// `meta.json` of a recording: how it was taken.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub label: Label,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// UTC, ISO 8601
    pub date: String,
    /// USB `vid:pid` of the IR camera, when known
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<String>,
    /// The ambient limit in effect (`--max-ambient`; Windows' 25 by default)
    pub max_ambient: f32,
    /// `linux_hello` version that recorded it
    pub version: String,
    /// The user whose template the frames were compared with
    pub user: String,
    /// Whether that user was enrolled in the store at the time (else no similarity scores)
    pub enrolled: bool,
    /// Whether the converted Windows Hello models ran
    pub windows_hello: bool,
    pub seconds: u64,
    /// Frame pairs recorded
    pub pairs: u32,
}

impl Meta {
    pub const FILE: &'static str = "meta.json";

    pub fn save(&self, dir: &Path) -> Result<()> {
        let path = dir.join(Self::FILE);
        std::fs::write(&path, serde_json::to_string_pretty(self)?).with_context(|| path.display().to_string())
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(Self::FILE);
        serde_json::from_slice(&std::fs::read(&path).with_context(|| path.display().to_string())?)
            .with_context(|| path.display().to_string())
    }
}

/// The current time as `2026-10-16T19:04:05Z`.
pub fn utc_now() -> String {
    utc_iso8601(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
}

/// Seconds since the Unix epoch as `YYYY-MM-DDThh:mm:ssZ`.
pub fn utc_iso8601(secs: u64) -> String {
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_round_trip() {
        for label in Label::ALL {
            assert_eq!(label.as_str().parse::<Label>(), Ok(label));
            assert_eq!(serde_json::to_string(&label).unwrap(), format!("\"{}\"", label.as_str()));
        }
        assert!("photo".parse::<Label>().is_err());
    }

    #[test]
    fn utc_iso_8601() {
        assert_eq!(utc_iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_iso8601(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc_iso8601(1_792_186_837), "2026-10-16T21:40:37Z");
    }
}
