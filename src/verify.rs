//! Post-write verification: hash the written region and compare to the ISO.

use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};

pub fn hash_file(f: &File, limit: Option<u64>) -> io::Result<String> {
    hash_file_p(f, limit, |_| {})
}

/// Like [`hash_file`], but reports bytes hashed after each chunk.
pub fn hash_file_p(
    mut f: &File,
    limit: Option<u64>,
    on_progress: impl Fn(u64),
) -> io::Result<String> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut remaining = limit;
    let mut done = 0u64;
    loop {
        let want = match remaining {
            Some(r) => (buf.len() as u64).min(r) as usize,
            None => buf.len(),
        };
        if want == 0 {
            break;
        }
        let n = f.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        done += n as u64;
        on_progress(done);
        remaining = remaining.map(|r| r.saturating_sub(n as u64));
    }
    Ok(hex(h.finalize()))
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{:02x}", b)).collect()
}

/// Compare sha256 of the first `iso_size` bytes of `target` against `iso_hash`.
/// `on_progress` is called with the number of bytes read so far.
pub fn verify(
    target: &File,
    iso_size: u64,
    iso_hash: &str,
    on_progress: impl Fn(u64),
) -> io::Result<bool> {
    Ok(hash_file_p(target, Some(iso_size), on_progress)? == iso_hash.to_lowercase())
}
