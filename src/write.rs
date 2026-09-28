//! Raw write loop + flush sequence.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};

#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
}

pub const BUF_SIZE: usize = 1024 * 1024;

/// Write `total` bytes from `src` to `dst` (1 MiB chunks), then
/// fsync + BLKFLSBUF (block devices only).
///
/// Note: no system-wide `sync()` here — it flushes *every* filesystem and
/// can block for minutes on boxes with FUSE/network mounts, stalling the
/// flash→verify transition. `sync_all` on the device + BLKFLSBUF are enough
/// to guarantee the data is on the target disk.
pub fn flash(
    mut src: &File,
    mut dst: &File,
    total: u64,
    on_progress: impl Fn(&Progress) -> io::Result<()>,
) -> io::Result<()> {
    // The fd may already be at EOF (e.g. the ISO was hashed through this
    // same handle first) — always start at the beginning.
    src.seek(SeekFrom::Start(0))?;
    let mut buf = vec![0u8; BUF_SIZE.min(total as usize).max(1)];
    let mut done = 0u64;
    while done < total {
        let want = (buf.len() as u64).min(total - done) as usize;
        let mut filled = 0usize;
        while filled < want {
            let n = src.read(&mut buf[filled..])?;
            if n == 0 {
                // Diagnose: where are we, and what does the kernel say the
                // file is?
                let pos = src.stream_position().unwrap_or(u64::MAX);
                let len = src.metadata().map(|m| m.len()).unwrap_or(u64::MAX);
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("unexpected EOF: at byte {pos}, file size {len}, wanted {total}"),
                ));
            }
            filled += n;
        }
        dst.write_all(&buf[..want])?;
        done += want as u64;
        on_progress(&Progress { done, total })?;
    }
    dst.sync_all()?;
    let _ = blkflush(dst); // best-effort; files don't have it
    Ok(())
}

/// `BLKFLSBUF` ioctl — ask the kernel to drop cached buffers for the device.
/// Ignored for regular files (used as test stand-ins for block devices).
fn blkflush(f: &File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    const BLKFLSBUF: u64 = 0x1261;
    let rc = unsafe { libc::ioctl(f.as_raw_fd(), BLKFLSBUF) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
