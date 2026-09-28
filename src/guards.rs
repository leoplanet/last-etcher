//! Layer 2 of the safety model: the exclusion set.
//!
//! A disk that passed USB enumeration is still refused (with a visible reason)
//! if it is the root disk, currently mounted, used as swap, or virtual.

use crate::probe::Sysfs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exclusion {
    RootDisk,
    Mounted,
    Swap,
    Virtual,
}

impl Exclusion {
    pub fn reason(self) -> &'static str {
        match self {
            Exclusion::RootDisk => "this is your system disk",
            Exclusion::Mounted => "in use: mounted (unmount it, then restart etcher)",
            Exclusion::Swap => "in use: swap",
            Exclusion::Virtual => "virtual device",
        }
    }
}

pub struct Guards {
    root_disk: Option<String>,
    mounted: Vec<String>,
    swap: Vec<String>,
    /// (whole disk, mountpoint) pairs from /proc/self/mounts.
    mount_points: Vec<(String, String)>,
}

impl Guards {
    /// Production constructor: reads `/proc/self/mounts`, `/proc/swaps`, and
    /// resolves the disk backing `/`.
    pub fn new(sysfs: Sysfs) -> Self {
        let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
        let swaps = std::fs::read_to_string("/proc/swaps").unwrap_or_default();
        let root_disk = Self::root_disk(&sysfs, &mounts);
        let (mounted, mount_points) = Self::mounted_disks(&sysfs, &mounts);
        let swap = Self::swap_disks(&sysfs, &swaps);
        Self { root_disk, mounted, swap, mount_points }
    }

    /// The disk backing `/`: statfs first, then the mount entry for `/`
    /// (covers roots on dm/LVM, where statfs reports no block device).
    fn root_disk(sysfs: &Sysfs, mounts: &str) -> Option<String> {
        if let Some(d) = Self::resolve_root_disk_statfs(sysfs) {
            return Some(d);
        }
        for line in mounts.lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() >= 2 && f[1] == "/" {
                if let Some(d) = Self::resolve_device(sysfs, f[0]) {
                    return Some(d);
                }
            }
        }
        None
    }

    /// Resolve a device path as it appears in mounts/swaps; follows
    /// `/dev/mapper/<name>` to its `dm-N` device.
    fn resolve_device(sysfs: &Sysfs, dev: &str) -> Option<String> {
        let trimmed = dev.trim_start_matches("/dev/");
        if trimmed.starts_with("mapper/") {
            let link = std::fs::read_link(format!("/dev/mapper/{}", trimmed.strip_prefix("mapper/").unwrap())).ok()?;
            let dm = link.file_name()?.to_string_lossy().into_owned();
            return sysfs.whole_disk_of(&dm);
        }
        sysfs.whole_disk_of(dev)
    }

    /// Test constructor: everything injected.
    #[cfg(test)]
    pub fn from_texts(sysfs: Sysfs, root_disk: Option<&str>, mounts: &str, swaps: &str) -> Self {
        let (mounted, mount_points) = Self::mounted_disks(&sysfs, mounts);
        let swap = Self::swap_disks(&sysfs, swaps);
        Self {
            root_disk: root_disk.map(str::to_string),
            mounted,
            swap,
            mount_points,
        }
    }

    /// Mountpoints currently mounted from `disk`.
    pub fn mount_points_of(&self, disk: &str) -> Vec<String> {
        self.mount_points
            .iter()
            .filter(|(d, _)| d == disk)
            .map(|(_, p)| p.clone())
            .collect()
    }

    /// `Some(reason)` if `name` must never be written.
    pub fn exclusion(&self, name: &str) -> Option<Exclusion> {
        if self.root_disk.as_deref() == Some(name) {
            return Some(Exclusion::RootDisk);
        }
        if self.mounted.iter().any(|d| d == name) {
            return Some(Exclusion::Mounted);
        }
        if self.swap.iter().any(|d| d == name) {
            return Some(Exclusion::Swap);
        }
        if name.starts_with("loop")
            || name.starts_with("ram")
            || name.starts_with("zram")
            || name.starts_with("dm-")
        {
            return Some(Exclusion::Virtual);
        }
        None
    }

    fn resolve_root_disk_statfs(sysfs: &Sysfs) -> Option<String> {
        let cpath = std::ffi::CString::new("/").ok()?;
        let mut sb: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::stat(cpath.as_ptr(), &mut sb) } != 0 {
            return None;
        }
        let dev = sb.st_dev as u64;
        let major = ((dev >> 8) & 0xfff) | ((dev >> 20) & 0xfff00);
        let minor = (dev & 0xff) | ((dev >> 12) & 0x3ff00);
        sysfs.whole_disk_of_major_minor(major as u32, minor as u32)
    }

    fn mounted_disks(sysfs: &Sysfs, text: &str) -> (Vec<String>, Vec<(String, String)>) {
        let mut out = Vec::new();
        let mut points = Vec::new();
        for line in text.lines() {
            // /proc/self/mounts: field 1 = device, field 2 = mountpoint
            let mut f = line.split_whitespace();
            let (dev, point) = (f.next().unwrap_or(""), f.next().unwrap_or(""));
            if let Some(disk) = Self::resolve_device(sysfs, dev) {
                if !out.contains(&disk) {
                    out.push(disk.clone());
                }
                if !point.is_empty() {
                    points.push((disk, point.to_string()));
                }
            }
        }
        (out, points)
    }

    fn swap_disks(sysfs: &Sysfs, text: &str) -> Vec<String> {
        let mut out = Vec::new();
        for line in text.lines() {
            let dev = line.split_whitespace().next().unwrap_or("");
            if let Some(disk) = Self::resolve_device(sysfs, dev) {
                if !out.contains(&disk) {
                    out.push(disk);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::Path;

    fn fixture(root: &Path) {
        // USB disk sdb (candidate)
        fs::create_dir_all(root.join("usb1/1-1/1-1:1.0")).unwrap();
        fs::create_dir_all(root.join("class/block/sdb")).unwrap();
        fs::write(root.join("class/block/sdb/size"), "31250000").unwrap();
        symlink(
            "../../../usb1/1-1/1-1:1.0",
            root.join("class/block/sdb/device"),
        )
        .unwrap();
        // USB disk sdc with a partition sdc1 (so mounts can reference it)
        fs::create_dir_all(root.join("usb2/2-1/2-1:1.0")).unwrap();
        fs::create_dir_all(root.join("class/block/sdc")).unwrap();
        fs::write(root.join("class/block/sdc/size"), "31250000").unwrap();
        symlink(
            "../../../usb2/2-1/2-1:1.0",
            root.join("class/block/sdc/device"),
        )
        .unwrap();
        fs::create_dir_all(root.join("class/block/sdc1")).unwrap();
        fs::write(root.join("class/block/sdc1/partition"), "").unwrap();
        // USB disk sdd (swap candidate)
        fs::create_dir_all(root.join("usb3/3-1/3-1:1.0")).unwrap();
        fs::create_dir_all(root.join("class/block/sdd")).unwrap();
        fs::write(root.join("class/block/sdd/size"), "31250000").unwrap();
        symlink(
            "../../../usb3/3-1/3-1:1.0",
            root.join("class/block/sdd/device"),
        )
        .unwrap();
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("etcher-guards-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn mounted_partition_excludes_whole_disk() {
        let d = tmp("mounted");
        fixture(&d);
        let sys = Sysfs::new(&d);
        let mounts = "udev /dev devtmpfs rw 0 0\n/dev/sdc1 /mnt/usb ext4 rw 0 0\ntmpfs /tmp tmpfs rw 0 0\n";
        let g = Guards::from_texts(sys, None, mounts, "");
        assert_eq!(g.exclusion("sdc"), Some(Exclusion::Mounted));
        assert_eq!(g.exclusion("sdb"), None);
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn swap_disk_excluded() {
        let d = tmp("swap");
        fixture(&d);
        let sys = Sysfs::new(&d);
        let swaps = "Filename\tType\tSize\tUsed\tPriority\n/dev/sdd\tpartition\t8191996\t0\t-2\n";
        let g = Guards::from_texts(sys, None, "", swaps);
        assert_eq!(g.exclusion("sdd"), Some(Exclusion::Swap));
        assert_eq!(g.exclusion("sdb"), None);
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn root_disk_excluded() {
        let d = tmp("root");
        fixture(&d);
        let sys = Sysfs::new(&d);
        let g = Guards::from_texts(sys, Some("sdb"), "", "");
        assert_eq!(g.exclusion("sdb"), Some(Exclusion::RootDisk));
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn virtual_names_excluded() {
        let d = tmp("virtual");
        fixture(&d);
        let sys = Sysfs::new(&d);
        let g = Guards::from_texts(sys, None, "", "");
        assert_eq!(g.exclusion("loop0"), Some(Exclusion::Virtual));
        assert_eq!(g.exclusion("dm-2"), Some(Exclusion::Virtual));
        assert_eq!(g.exclusion("ram0"), Some(Exclusion::Virtual));
        assert_eq!(g.exclusion("sdb"), None);
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn real_guards_build_without_panic() {
        // Smoke test against the live system: must not panic, must identify a
        // root disk (this machine has one, via dm/mapper).
        let g = Guards::new(Sysfs::default());
        assert!(g.root_disk.is_some());
    }
}

