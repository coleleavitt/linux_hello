//! Lid-closed suppression (`IsLidClosedScenario`, `ShouldForbidExternalCameras`): with the lid
//! closed, a built-in camera sees the keyboard, not the user, so face login is not offered.

use std::fs;

/// Whether an ACPI lid switch reports "closed". `false` without a lid switch (desktops).
pub fn lid_closed() -> bool {
    let Ok(lids) = fs::read_dir("/proc/acpi/button/lid") else { return false };
    lids.flatten().any(|lid| fs::read_to_string(lid.path().join("state")).is_ok_and(|s| state_closed(&s)))
}

/// `state:      closed` → true.
fn state_closed(content: &str) -> bool {
    content.split_whitespace().last() == Some("closed")
}

/// Whether the V4L2 `device` (e.g. `/dev/video2`) is built into the machine: its USB device's
/// sysfs `removable` attribute is `fixed`. Unknown (`unknown`, no USB parent, no sysfs entry)
/// counts as built in, so a closed lid suppresses face login when in doubt.
pub fn built_in(device: &str) -> bool {
    crate::sysfs::usb_attribute(device, "removable").as_deref() != Some("removable")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lid_state() {
        assert!(state_closed("state:      closed\n"));
        assert!(!state_closed("state:      open\n"));
        assert!(!state_closed(""));
    }
}
