use clap::{Parser, Subcommand};
use etcher::{guards, gui, identity, iso, probe, verify, write};
use probe::{Disk, Sysfs};
use std::path::Path;

#[derive(Parser)]
#[command(
    name = "etcher",
    version,
    about = "Write a live ISO to a USB drive, with safety layers to protect other disks"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// List USB drives (and why other disks are excluded)
    List,
    /// Non-interactive flash: type the device name yourself, that is the confirmation
    Flash { iso: std::path::PathBuf, device: String },
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Some(Cmd::List) => list(),
        Some(Cmd::Flash { iso, device }) => flash_cli(&iso, &device),
        None => gui::run(),
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

fn describe(d: &Disk) -> String {
    let label = match (d.vendor.as_deref(), d.model.as_deref()) {
        (Some(v), Some(m)) if !m.is_empty() => format!("{v} {m}"),
        (Some(v), _) => v.to_string(),
        (_, Some(m)) if !m.is_empty() => m.to_string(),
        _ => "unknown model".to_string(),
    };
    format!("{} — {} ({})", d.name, label, human_size(d.size))
}

fn list() -> anyhow::Result<()> {
    let sys = Sysfs::default();
    let guards = guards::Guards::new(sys.clone());
    let disks = sys.whole_disks();
    if disks.is_empty() {
        println!("No block devices found.");
        return Ok(());
    }
    for d in &disks {
        match guards.exclusion(&d.name) {
            None if d.is_usb => println!("  {}  [USB, removable={}]", describe(d), d.removable),
            None => println!("  {}  [not USB — not offered]", describe(d)),
            Some(e) => println!("  {}  [excluded: {}]", describe(d), e.reason()),
        }
    }
    Ok(())
}

fn flash_cli(iso_path: &Path, device: &str) -> anyhow::Result<()> {
    let iso = iso::validate(iso_path)?;
    let sys = Sysfs::default();
    let disk = sys
        .disk(device)
        .ok_or_else(|| anyhow::anyhow!("{} is not a whole block device", device))?;
    anyhow::ensure!(
        disk.is_usb,
        "{} is not a USB disk — refusing",
        device
    );
    let guards = guards::Guards::new(sys.clone());
    if let Some(e) = guards.exclusion(&disk.name) {
        // The user explicitly chose this USB disk (typed its name / entered a
        // password). If it is only excluded because a partition is mounted —
        // e.g. the desktop auto-mounted it — unmount it and re-check.
        if e == guards::Exclusion::Mounted && disk.is_usb {
            for p in guards.mount_points_of(&disk.name) {
                eprintln!("unmounting {p}");
                let ok = std::process::Command::new("umount")
                    .arg(&p)
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
                    || std::process::Command::new("umount")
                        .arg("-l") // lazy, if something holds it open
                        .arg(&p)
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false);
                if !ok {
                    anyhow::bail!("cannot unmount {p} — close anything using it and retry");
                }
            }
            let guards = guards::Guards::new(sys.clone());
            if let Some(e) = guards.exclusion(&disk.name) {
                anyhow::bail!("{} is excluded: {}", device, e.reason());
            }
        } else {
            anyhow::bail!("{} is excluded: {}", device, e.reason());
        }
    }
    anyhow::ensure!(
        disk.size >= iso.size,
        "drive {} ({} ) is smaller than the ISO ({} )",
        device,
        human_size(disk.size),
        human_size(iso.size)
    );
    if !iso.is_iso9660 {
        eprintln!("warning: no ISO-9660 signature found; writing anyway");
    }
    let identity = identity::Identity::from(&disk);
    eprintln!(
        "Flashing {} ({} ) to {} — ALL DATA ON {} WILL BE ERASED",
        iso.path.display(),
        human_size(iso.size),
        describe(&disk),
        device
    );

    // Hash the ISO first (verification needs it).
    eprintln!("Hashing ISO…");
    let iso_file = std::fs::File::open(&iso.path)?;
    let iso_hash = verify::hash_file(&iso_file, None)?;

    // Re-verify identity, then write.
    let fresh = sys.disk(&disk.name);
    identity.verify(fresh.as_ref()).map_err(|e| anyhow::anyhow!(e))?;
    let dst = std::fs::OpenOptions::new()
        .write(true)
        .open(format!("/dev/{}", disk.name))
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::PermissionDenied => {
                let exe = std::env::current_exe().unwrap_or_default();
                anyhow::anyhow!("permission denied — re-run as: sudo {} flash {} {}", exe.display(), iso.path.display(), device)
            }
            _ => e.into(),
        })?;
    eprintln!("Writing…");
    write::flash(&iso_file, &dst, iso.size, |p| {
        eprint!("\r  {}/{} ({}%)", human_size(p.done), human_size(p.total), p.done * 100 / p.total);
        Ok(())
    })?;
    eprintln!();
    eprintln!("Verifying…");
    let dst = std::fs::File::open(format!("/dev/{}", disk.name))?;
    if verify::verify(&dst, iso.size, &iso_hash, |done| {
        eprint!("\r  {}/{} ({}%)", human_size(done), human_size(iso.size), done * 100 / iso.size);
    })? {
        eprintln!();
        eprintln!("Success: {} written to {} and verified.", iso.path.display(), device);
    } else {
        anyhow::bail!("verification FAILED — the drive does not match the ISO");
    }
    Ok(())
}
