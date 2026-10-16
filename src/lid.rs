//! Lid-closed suppression (`IsLidClosedScenario`, `ShouldForbidExternalCameras`): with the lid
//! closed, a built-in camera sees the keyboard, not the user, so face login is not offered.

use std::fs;
use std::path::Path;

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
    let name = Path::new(device).file_name().unwrap_or_default();
    removable(&Path::new("/sys/class/video4linux").join(name).join("device")).as_deref() != Some("removable")
}

/// The `removable` attribute of the first ancestor of the sysfs `device` link that has one: the
/// link points at the USB interface, its parent is the USB device.
fn removable(device: &Path) -> Option<String> {
    let path = fs::canonicalize(device).ok()?;
    path.ancestors().find_map(|dir| fs::read_to_string(dir.join("removable")).ok()).map(|s| s.trim().to_string())
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

    #[test]
    fn removable_comes_from_the_usb_device() {
        let root = std::env::temp_dir().join(format!("linux_hello_lid_{}", std::process::id()));
        let interface = root.join("usb3/3-4/3-4:1.2");
        fs::create_dir_all(&interface).unwrap();
        fs::write(root.join("usb3/removable"), "unknown\n").unwrap();
        fs::write(root.join("usb3/3-4/removable"), "fixed\n").unwrap();
        let link = root.join("device");
        std::os::unix::fs::symlink(&interface, &link).unwrap();
        assert_eq!(removable(&link).as_deref(), Some("fixed"));
        assert_eq!(removable(&root.join("missing")), None);
        fs::remove_dir_all(root).unwrap();
    }
}
