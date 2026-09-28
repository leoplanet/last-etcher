# LAST ETCHER

Write a live ISO to a USB drive, with safety layers that make it very hard to
destroy the wrong disk. A single-purpose, Balena-Etcher-style tool for Linux,
written in Rust.

## Build

```sh
cargo build --release
# binary: ./target/release/etcher
```

## Install (Arch / Omarchy)

An AUR package is provided in [`aur/last-etcher-git`](aur/last-etcher-git/PKGBUILD):

```sh
yay -S last-etcher-git   # or any AUR helper
```

## Usage

Interactive GUI (default):

```sh
cargo run --release              # during development
```

1. Browse to (or type) the ISO path.
2. Pick the USB drive (only USB whole-disks are selectable; everything else is
   greyed out with the reason).
3. Type the exact drive name to confirm, then hit FLASH.
4. Watch the progress bar, get a hash-verified success/failure.

Non-interactive (scripting):

```sh
etcher list                      # show drives and exclusion reasons
etcher flash /path/to/image.iso sdb
```

Writing to `/dev/<disk>` needs root. If the write is refused, the tool prints
the exact `sudo …` command to run.

## Safety model

A write happens only if **all four** layers pass:

1. **Enumeration** — only whole disks whose sysfs `device` path contains a
   `usbN` component are candidates. SATA/NVMe/virtio disks can never be
   written. Partitions (`sdb1`, `nvme0n1p2`) are never offered.
2. **Exclusion set** — even a USB disk is refused if it is the disk backing
   `/` (resolved via statfs, with a `/proc/self/mounts` fallback for
   dm/LVM roots), currently mounted, used as swap, or virtual (`loop*`,
   `ram*`, `zram*`, `dm-*`). Excluded disks are shown greyed out with the
   reason.
3. **Identity pinning** — the confirmed drive is pinned by sysfs path +
   serial + size and re-verified at write time. If you yank the stick (or a
   different stick takes the name) between confirmation and write, the write
   is refused.
4. **Type-to-confirm** — you must type the exact device name. No Enter-only,
   no "y", no timeout.

After the write: `fsync` + `BLKFLSBUF` + `sync`, then the first
`iso_size` bytes of the drive are hashed and compared to the ISO's sha256.
Verification failure is reported as a failure.

The Cancel button aborts a write cleanly; a partially written drive is simply
re-flashed from scratch.

## Tests

```sh
cargo test
```

- Unit tests run the exclusion/identity/probe logic against fixture sysfs
  trees (no hardware needed).
- An integration test runs the full write/verify pipeline against a regular
  file standing in for a block device.

## Manual hardware checklist

- [ ] Flash a real stick, boot it.
- [ ] Plug in a stick with a mounted data partition → confirm it is excluded
      with reason "in use: mounted".
- [ ] Run the tool from a live-USB session → confirm the boot stick is
      excluded (root disk).
- [ ] Yank the stick at the confirm screen → write refused.
- [ ] Run as non-root → permission error shows the `sudo` command.
