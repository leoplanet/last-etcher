//! Integration test: the write/verify pipeline against a regular file
//! standing in for a block device.

use std::io::{Seek, Write};

const ISO_SIZE: u64 = 10 * 1024 * 1024; // 10 MiB
const TARGET_SIZE: u64 = 20 * 1024 * 1024; // 20 MiB

fn deterministic_iso(path: &std::path::Path) {
    let mut f = std::fs::File::create(path).unwrap();
    let mut chunk = vec![0u8; 64 * 1024];
    let mut counter = 0u64;
    while (f.metadata().unwrap().len()) < ISO_SIZE {
        for (i, b) in chunk.iter_mut().enumerate() {
            *b = (counter.wrapping_add(i as u64).wrapping_mul(0x9E3779B97F4A7C15)) as u8;
        }
        f.write_all(&chunk).unwrap();
        counter += 1;
    }
    let _ = f.set_len(ISO_SIZE);
}

#[test]
fn flash_to_file_target_and_verify() {
    let dir = std::env::temp_dir().join(format!("etcher-int-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let iso_path = dir.join("image.iso");
    let target_path = dir.join("target.img");
    deterministic_iso(&iso_path);

    // Target starts all zeros.
    let target = std::fs::File::create(&target_path).unwrap();
    target.set_len(TARGET_SIZE).unwrap();
    drop(target);

    let iso = std::fs::File::open(&iso_path).unwrap();
    let iso_hash = etcher::verify::hash_file(&iso, None).unwrap();
    // Reopen: hash_file left the previous handle at EOF.
    let iso = std::fs::File::open(&iso_path).unwrap();

    let target = std::fs::OpenOptions::new().write(true).open(&target_path).unwrap();
    let last = std::cell::Cell::new(0u64);
    etcher::write::flash(&iso, &target, ISO_SIZE, |p| {
        last.set(p.done);
        Ok(())
    })
    .unwrap();
    assert_eq!(last.get(), ISO_SIZE);

    // Written region matches the ISO byte-for-byte.
    let iso_bytes = std::fs::read(&iso_path).unwrap();
    let target_bytes = std::fs::read(&target_path).unwrap();
    assert_eq!(target_bytes.len() as u64, TARGET_SIZE);
    assert_eq!(&target_bytes[..ISO_SIZE as usize], &iso_bytes[..]);
    // Tail beyond the image is untouched.
    assert!(target_bytes[ISO_SIZE as usize..].iter().all(|&b| b == 0));

    // Hash verify passes on a matching target…
    let target = std::fs::File::open(&target_path).unwrap();
    assert!(etcher::verify::verify(&target, ISO_SIZE, &iso_hash, |_| {}).unwrap());

    // …and fails on a corrupted target.
    let mut f = std::fs::OpenOptions::new().write(true).open(&target_path).unwrap();
    f.seek(std::io::SeekFrom::Start(ISO_SIZE / 2)).unwrap();
    f.write_all(b"corrupt").unwrap();
    drop(f);
    let target = std::fs::File::open(&target_path).unwrap();
    assert!(!etcher::verify::verify(&target, ISO_SIZE, &iso_hash, |_| {}).unwrap());

    std::fs::remove_dir_all(dir).unwrap();
}

/// Regression: the CLI hashes the ISO and then writes from the *same* file
/// handle. hash_file leaves the fd at EOF; flash must seek back to 0.
#[test]
fn flash_from_same_handle_after_hash() {
    let dir = std::env::temp_dir().join(format!("etcher-samefd-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()));
    std::fs::create_dir_all(&dir).unwrap();
    let iso_path = dir.join("image.iso");
    let target_path = dir.join("target.img");
    deterministic_iso(&iso_path);

    let target = std::fs::File::create(&target_path).unwrap();
    target.set_len(TARGET_SIZE).unwrap();
    drop(target);

    let iso = std::fs::File::open(&iso_path).unwrap();
    let iso_hash = etcher::verify::hash_file(&iso, None).unwrap();
    // No reopen — this is the exact CLI sequence.
    let target = std::fs::OpenOptions::new().write(true).open(&target_path).unwrap();
    etcher::write::flash(&iso, &target, ISO_SIZE, |_| Ok(())).unwrap();

    let target = std::fs::File::open(&target_path).unwrap();
    assert!(etcher::verify::verify(&target, ISO_SIZE, &iso_hash, |_| {}).unwrap());

    std::fs::remove_dir_all(dir).unwrap();
}
