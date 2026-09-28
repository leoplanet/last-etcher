//! ISO validation: regular file, non-empty, ISO-9660 signature (advisory).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct IsoInfo {
    pub path: PathBuf,
    pub size: u64,
    /// ISO 9660 Primary Volume Descriptor signature found at 2048*16+1.
    /// Absence is a warning, not a failure — some vendor images are odd.
    pub is_iso9660: bool,
}

pub fn validate(path: &Path) -> anyhow::Result<IsoInfo> {
    let meta = std::fs::metadata(path)?;
    anyhow::ensure!(meta.is_file(), "{} is not a regular file", path.display());
    let size = meta.len();
    anyhow::ensure!(size > 0, "{} is empty", path.display());
    Ok(IsoInfo {
        path: path.to_path_buf(),
        size,
        is_iso9660: check_signature(path, size),
    })
}

fn check_signature(path: &Path, size: u64) -> bool {
    const OFFSET: u64 = 2048 * 16 + 1;
    if size <= OFFSET + 5 {
        return false;
    }
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    if f.seek(SeekFrom::Start(OFFSET)).is_err() {
        return false;
    }
    let mut buf = [0u8; 5];
    f.read_exact(&mut buf).map(|_| buf == *b"CD001").unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("etcher-iso-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rejects_missing_and_empty() {
        assert!(validate(Path::new("/nonexistent.iso")).is_err());
        let d = tmp("empty");
        let empty = d.join("empty.iso");
        std::fs::write(&empty, "").unwrap();
        assert!(validate(&empty).is_err());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn detects_iso9660_signature() {
        let d = tmp("sig");
        let p = d.join("good.iso");
        let mut f = std::fs::File::create(&p).unwrap();
        let mut zeros = vec![0u8; (2048 * 16 + 1) as usize];
        zeros.extend_from_slice(b"CD001");
        zeros.extend_from_slice(&[0u8; 2048]);
        f.write_all(&zeros).unwrap();
        let info = validate(&p).unwrap();
        assert!(info.is_iso9660);
        assert_eq!(info.size, zeros.len() as u64);

        // No signature → accepted with is_iso9660 = false (warn, don't fail).
        let p2 = d.join("weird.iso");
        std::fs::write(&p2, vec![0u8; 4096]).unwrap();
        let info2 = validate(&p2).unwrap();
        assert!(!info2.is_iso9660);
        std::fs::remove_dir_all(d).unwrap();
    }
}
