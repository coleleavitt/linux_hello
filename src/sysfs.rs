//! USB attributes of a V4L2 device, read from sysfs: `/sys/class/video4linux/<videoN>/device`
//! points at the USB interface; the USB device is the first ancestor with the attribute.

use std::fs;
use std::path::{Path, PathBuf};

const CLASS: &str = "/sys/class/video4linux";

/// Every V4L2 device node, `/dev/video0`, `/dev/video1`, ... in numeric order.
pub fn video_nodes() -> Vec<String> {
    let Ok(entries) = fs::read_dir(CLASS) else { return Vec::new() };
    let mut numbers: Vec<u32> =
        entries.flatten().filter_map(|e| e.file_name().to_str()?.strip_prefix("video")?.parse().ok()).collect();
    numbers.sort_unstable();
    numbers.iter().map(|n| format!("/dev/video{n}")).collect()
}

/// The sysfs directory of the USB interface behind `device` (e.g. `.../usb3/3-4/3-4:1.2` for
/// `/dev/video2`): the nodes of one camera function (video and metadata) share it.
pub fn usb_interface(device: &str) -> Option<PathBuf> {
    fs::canonicalize(device_link(device)).ok()
}

/// The `name` attribute (e.g. `removable`, `idVendor`) of the USB device behind `device`
/// (e.g. `/dev/video2`), trimmed. `None` without such an attribute (no USB parent, no sysfs entry).
pub fn usb_attribute(device: &str, name: &str) -> Option<String> {
    attribute(&device_link(device), name)
}

/// `idVendor:idProduct` of the USB device behind `device`, e.g. `30c9:00f3`.
pub fn usb_id(device: &str) -> Option<String> {
    Some(format!("{}:{}", usb_attribute(device, "idVendor")?, usb_attribute(device, "idProduct")?))
}

fn device_link(device: &str) -> PathBuf {
    let node = Path::new(device).file_name().unwrap_or_default();
    Path::new(CLASS).join(node).join("device")
}

/// `name` of the first ancestor of the sysfs `device` link that has it.
fn attribute(device: &Path, name: &str) -> Option<String> {
    let path = fs::canonicalize(device).ok()?;
    path.ancestors().find_map(|dir| fs::read_to_string(dir.join(name)).ok()).map(|s| s.trim().to_string())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_come_from_the_usb_device() {
        let root = std::env::temp_dir().join(format!("linux_hello_sysfs_{}", std::process::id()));
        let interface = root.join("usb3/3-4/3-4:1.2");
        fs::create_dir_all(&interface).unwrap();
        fs::write(root.join("usb3/removable"), "unknown\n").unwrap();
        fs::write(root.join("usb3/3-4/removable"), "fixed\n").unwrap();
        fs::write(root.join("usb3/3-4/idVendor"), "30c9\n").unwrap();
        let link = root.join("device");
        std::os::unix::fs::symlink(&interface, &link).unwrap();
        assert_eq!(attribute(&link, "removable").as_deref(), Some("fixed"));
        assert_eq!(attribute(&link, "idVendor").as_deref(), Some("30c9"));
        assert_eq!(attribute(&link, "idProduct"), None);
        assert_eq!(attribute(&root.join("missing"), "removable"), None);
        fs::remove_dir_all(root).unwrap();
    }
}
