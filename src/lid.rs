//! Laptop lid state detection.
//!
//! Detects whether the laptop lid is open or closed by reading from
//! `/proc/acpi/button/lid/*/state`. Used to suppress face authentication
//! when the lid is closed (internal camera is covered).

use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::debug;

/// How long to cache the lid state before re-reading.
const CACHE_TTL: Duration = Duration::from_secs(1);

/// Lid state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LidState {
    Open,
    Closed,
    /// No lid switch found (desktop, or unsupported hardware).
    Unknown,
}

/// Cached lid state with timestamp.
struct CachedState {
    state: LidState,
    checked_at: Instant,
}

static CACHE: Mutex<Option<CachedState>> = Mutex::new(None);

/// Get the current lid state, using a 1-second cache.
pub fn get_lid_state() -> LidState {
    let now = Instant::now();

    // Check cache first
    {
        let cache = CACHE.lock().unwrap();
        if let Some(ref cached) = *cache {
            if now.duration_since(cached.checked_at) < CACHE_TTL {
                return cached.state;
            }
        }
    }

    // Read fresh state
    let state = read_lid_state();
    debug!(?state, "lid state");

    // Update cache
    {
        let mut cache = CACHE.lock().unwrap();
        *cache = Some(CachedState {
            state,
            checked_at: now,
        });
    }

    state
}

/// Read the lid state from procfs.
fn read_lid_state() -> LidState {
    // Try common lid switch paths
    let candidates = [
        "/proc/acpi/button/lid/LID/state",
        "/proc/acpi/button/lid/LID0/state",
        "/proc/acpi/button/lid/LID1/state",
    ];

    for path in candidates {
        if let Some(state) = read_lid_file(path) {
            return state;
        }
    }

    // Also try globbing the lid directory
    if let Ok(entries) = fs::read_dir("/proc/acpi/button/lid") {
        for entry in entries.flatten() {
            let state_path = entry.path().join("state");
            if let Some(state) = read_lid_file(&state_path) {
                return state;
            }
        }
    }

    LidState::Unknown
}

/// Read and parse a single lid state file.
fn read_lid_file(path: impl AsRef<Path>) -> Option<LidState> {
    let content = fs::read_to_string(path.as_ref()).ok()?;
    parse_lid_state(&content)
}

/// Parse the lid state from the file content.
///
/// Format: `state:      open` or `state:      closed` (with variable whitespace).
fn parse_lid_state(content: &str) -> Option<LidState> {
    let content = content.trim().to_lowercase();

    if content.contains("open") {
        Some(LidState::Open)
    } else if content.contains("closed") {
        Some(LidState::Closed)
    } else {
        None
    }
}

/// Check if authentication should be suppressed due to lid being closed.
///
/// Returns `true` if the lid is closed and the camera is internal.
/// External cameras can still be used with the lid closed.
pub fn should_suppress_auth(camera_is_internal: bool) -> bool {
    if !camera_is_internal {
        return false;
    }

    matches!(get_lid_state(), LidState::Closed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_lid_state() {
        assert_eq!(parse_lid_state("state:      open"), Some(LidState::Open));
        assert_eq!(parse_lid_state("state:      closed"), Some(LidState::Closed));
        assert_eq!(parse_lid_state("state: open\n"), Some(LidState::Open));
        assert_eq!(parse_lid_state("STATE: CLOSED"), Some(LidState::Closed));
        assert_eq!(parse_lid_state("garbage"), None);
    }

    #[test]
    fn test_should_suppress_auth() {
        // External camera: never suppress
        assert!(!should_suppress_auth(false));
    }
}
