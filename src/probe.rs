//! sysfs enumeration of block devices.
//!
//! Everything here takes a sysfs root (default `/sys`) so the safety logic
//! can be unit-tested against fixture trees.

use std::fs;
use std::path::{Path, PathBuf};

/// A whole block device as seen in sysfs.
#[derive(Debug, Clone)]
pub struct Disk {
    pub name: String,
    /// Capacity in bytes.
    pub size: u64,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    /// Advisory only — some USB enclosures report 0.
    pub removable: bool,
    /// Canonicalized path of the device directory; stable per physical device.
    pub sysfs_path: String,
    /// Kernel says this disk hangs off a USB controller.
    pub is_usb: bool,
}

#[derive(Clone)]
pub struct Sysfs {
    pub root: PathBuf,
}

impl Sysfs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn block(&self) -> PathBuf {
        self.root.join("class/block")
    }

    /// All whole disks (partitions excluded).
    pub fn whole_disks(&self) -> Vec<Disk> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(self.block()) else {
            return out;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if self.is_partition(&name) {
                continue;
            }
            if let Some(d) = self.disk(&name) {
                out.push(d);
            }
        }
        out
    }

    pub fn is_partition(&self, name: &str) -> bool {
        self.block().join(name).join("partition").exists()
    }

    /// Read one whole disk; `None` if `name` is a partition or unknown.
    pub fn disk(&self, name: &str) -> Option<Disk> {
        let dir = self.block().join(name);
        if self.is_partition(name) || !dir.join("size").exists() {
            return None;
        }
        let size_sectors: u64 = fs::read_to_string(dir.join("size")).ok()?.trim().parse().ok()?;
        let device_path = fs::canonicalize(dir.join("device")).ok()?;
        let is_usb = device_path.components().any(|c| {
            let s = c.as_os_str().to_string_lossy();
            s.len() > 3 && s.starts_with("usb") && s[3..].chars().all(|c| c.is_ascii_digit())
        });
        let read = |p: &Path| {
            fs::read_to_string(p)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        Some(Disk {
            name: name.to_string(),
            size: size_sectors * 512,
            vendor: read(&device_path.join("vendor")),
            model: read(&device_path.join("model")),
            serial: read(&device_path.join("serial")),
            removable: read(&dir.join("removable")).as_deref() == Some("1"),
            sysfs_path: device_path.to_string_lossy().into_owned(),
            is_usb,
        })
    }

    /// Resolve a device name (`/dev/sda1`, `sda1`, `nvme0n1p2`, `dm-0`) to its
    /// whole-disk name.
    pub fn whole_disk_of(&self, dev: &str) -> Option<String> {
        let name = dev.trim_start_matches("/dev/").trim();
        if name.is_empty() || !self.block().join(name).exists() {
            return None;
        }
        if self.is_partition(name) {
            Self::parent_disk(name)
        } else {
            Some(name.to_string())
        }
    }

    /// Resolve a major:minor (from statfs) to a whole-disk name.
    pub fn whole_disk_of_major_minor(&self, major: u32, minor: u32) -> Option<String> {
        let p = self.root.join("dev/block").join(format!("{major}:{minor}"));
        let name = fs::canonicalize(&p).ok()?.file_name()?.to_string_lossy().into_owned();
        if self.is_partition(&name) {
            Self::parent_disk(&name)
        } else {
            Some(name)
        }
    }

    /// `nvme0n1p2` -> `nvme0n1`, `sda1` -> `sda`.
    fn parent_disk(name: &str) -> Option<String> {
        if let Some(idx) = name.rfind('p') {
            if idx > 0 && name[idx + 1..].chars().all(|c| c.is_ascii_digit()) {
                let prefix = &name[..idx];
                if !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_alphanumeric()) {
                    return Some(prefix.to_string());
                }
            }
        }
        let s = name.trim_end_matches(|c: char| c.is_ascii_digit());
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    }

}

impl Default for Sysfs {
    fn default() -> Self {
        Self::new("/sys")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    fn mkpath(p: &Path) {
        fs::create_dir_all(p).unwrap();
    }

    /// Build a fixture sysfs tree with one USB disk `sdb` and one SATA disk `sda`.
    fn fixture(root: &Path) {
        // sda: SATA whole disk
        mkpath(&root.join("class/block/sda/device"));
        fs::write(root.join("class/block/sda/size"), "1000").unwrap();
        symlink("../../sata/ahci0/sda", root.join("class/block/sda/device/link")).unwrap();
        // sdb: USB whole disk
        mkpath(&root.join("usb1/1-1/1-1:1.0"));
        mkpath(&root.join("class/block/sdb"));
        fs::write(root.join("class/block/sdb/size"), "31250000").unwrap();
        fs::write(root.join("class/block/sdb/removable"), "1").unwrap();
        fs::write(root.join("usb1/1-1/1-1:1.0/vendor"), "SanDisk").unwrap();
        fs::write(root.join("usb1/1-1/1-1:1.0/model"), "Ultra 3.0").unwrap();
        fs::write(root.join("usb1/1-1/1-1:1.0/serial"), "S123").unwrap();
        symlink(
            "../../../usb1/1-1/1-1:1.0",
            root.join("class/block/sdb/device"),
        )
        .unwrap();
        // sdb1: partition of sdb
        mkpath(&root.join("class/block/sdb1"));
        fs::write(root.join("class/block/sdb1/partition"), "").unwrap();
    }

    #[test]
    fn enumerates_whole_disks_and_flags_usb() {
        let dir = tempfile_dir("enum");
        fixture(&dir);
        let sys = Sysfs::new(&dir);
        let disks = sys.whole_disks();
        assert_eq!(disks.len(), 2);
        let sda = disks.iter().find(|d| d.name == "sda").unwrap();
        assert!(!sda.is_usb);
        let sdb = disks.iter().find(|d| d.name == "sdb").unwrap();
        assert!(sdb.is_usb);
        assert!(sdb.removable);
        assert_eq!(sdb.size, 31_250_000 * 512);
        assert_eq!(sdb.vendor.as_deref(), Some("SanDisk"));
        assert_eq!(sdb.model.as_deref(), Some("Ultra 3.0"));
        assert_eq!(sdb.serial.as_deref(), Some("S123"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn whole_disk_of_resolves_partitions() {
        let dir = tempfile_dir("resolve");
        fixture(&dir);
        let sys = Sysfs::new(&dir);
        assert_eq!(sys.whole_disk_of("/dev/sdb1").as_deref(), Some("sdb"));
        assert_eq!(sys.whole_disk_of("sda").as_deref(), Some("sda"));
        assert_eq!(sys.whole_disk_of("nvme0n1p2"), None); // not in fixture
        assert_eq!(Sysfs::parent_disk("nvme0n1p2").as_deref(), Some("nvme0n1"));
        assert_eq!(Sysfs::parent_disk("sda1").as_deref(), Some("sda"));
        fs::remove_dir_all(dir).unwrap();
    }

    fn tempfile_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("etcher-probe-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        mkpath(&d);
        d
    }
}
