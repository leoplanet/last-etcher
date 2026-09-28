//! Layer 3 of the safety model: identity pinning.
//!
//! `/dev/sdX` names can be reshuffled by hotplug between selection and write.
//! The confirmed target is pinned by sysfs path + serial + size, and re-verified
//! at write time.

use crate::probe::Disk;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub sysfs_path: String,
    pub serial: Option<String>,
    pub size: u64,
}

impl Identity {
    pub fn from(disk: &Disk) -> Self {
        Self {
            name: disk.name.clone(),
            sysfs_path: disk.sysfs_path.clone(),
            serial: disk.serial.clone(),
            size: disk.size,
        }
    }

    /// Re-verify a freshly probed disk against the pinned identity.
    /// `None` disk means the device is gone.
    pub fn verify(&self, disk: Option<&Disk>) -> Result<(), String> {
        let disk = disk.ok_or_else(|| format!("{} is no longer present", self.name))?;
        if disk.sysfs_path != self.sysfs_path {
            return Err(format!(
                "{} is now a different device (sysfs path changed)",
                self.name
            ));
        }
        if disk.serial != self.serial {
            return Err(format!("{} serial mismatch", self.name));
        }
        if disk.size != self.size {
            return Err(format!("{} size changed", self.name));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(name: &str, sysfs: &str, serial: Option<&str>, size: u64) -> Disk {
        Disk {
            name: name.into(),
            size,
            vendor: None,
            model: None,
            serial: serial.map(str::to_string),
            removable: true,
            sysfs_path: sysfs.into(),
            is_usb: true,
        }
    }

    #[test]
    fn matching_identity_passes() {
        let id = Identity::from(&disk("sdb", "/usb1/1-1/1-1:1.0", Some("S1"), 1000));
        let fresh = disk("sdb", "/usb1/1-1/1-1:1.0", Some("S1"), 1000);
        assert!(id.verify(Some(&fresh)).is_ok());
    }

    #[test]
    fn gone_device_fails() {
        let id = Identity::from(&disk("sdb", "/usb1/1-1/1-1:1.0", Some("S1"), 1000));
        assert!(id.verify(None).is_err());
    }

    #[test]
    fn swapped_device_fails() {
        let id = Identity::from(&disk("sdb", "/usb1/1-1/1-1:1.0", Some("S1"), 1000));
        let other = disk("sdb", "/usb1/1-2/1-2:1.0", Some("S2"), 2000);
        assert!(id.verify(Some(&other)).is_err());
    }
}
