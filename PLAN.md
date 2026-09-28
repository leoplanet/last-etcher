# USB Etcher — Plan

A single-purpose Linux tool, written in Rust, that writes a live ISO image to a USB
drive. One drive at a time. No fleet features, no partitioning, no extras.

The defining requirement is **safety**: the tool must only ever write to a USB
drive the user explicitly confirmed, and must never touch the disk the OS is
running on (or any non-USB disk).

---

## 1. Goals / Non-goals

**Goals**
- Detect USB block devices and show size, vendor, model.
- Pick an ISO file, validate it.
- Write the ISO raw to the selected USB disk (whole-disk, `dd`-style).
- Show progress (bytes, speed, ETA).
- Verify the write by hashing the written region and comparing to the ISO.
- Force explicit, unambiguous user confirmation before any write.
- Refuse to write to anything that is not a USB whole-disk, with a clear reason.

**Non-goals**
- Multiple drives at once, fleet/remote operation.
- GUI (this is a terminal TUI).
- Formatting the leftover space after the ISO, partitioning, MBR/GPT surgery.
- Windows/macOS support.
- Resuming an interrupted write (a restart re-writes from byte 0 — safe by construction).

---

## 2. Architecture

Single binary, one crate. TUI with **ratatui + crossterm** (the same stack as
most modern Rust CLIs; no new dependencies beyond what's listed).

```
etcher/
├── main.rs          # arg parsing, privilege check, TUI entry
├── probe.rs         # sysfs enumeration → candidate USB disks
├── guards.rs        # exclusion logic: root disk, mounted disks, swap, virtual
├── identity.rs      # pin a device by sysfs path + serial, re-verify at write time
├── iso.rs           # ISO validation (size, ISO-9660 signature)
├── write.rs         # raw write loop, flush, progress events
├── verify.rs        # hash written region, compare to ISO hash
├── confirm.rs       # type-the-device-name confirmation
└── tui.rs           # 3-step wizard: pick ISO → pick drive → confirm
```

Dependencies: `ratatui`, `crossterm`, `clap`, `sha2`, `nix` (ioctl for
`BLKFLSBUF`, `sync`), `anyhow`. That's it.

### 2.1 Flow

```
start → privilege check (re-exec via sudo if /dev not writable)
      → TUI step 1: choose ISO (file list / type path) → validate
      → TUI step 2: choose USB drive (only safe candidates listed)
      → TUI step 3: summary + type-the-device-name confirmation
      → re-verify identity (device still present, same sysfs path/serial/size)
      → write → flush → sync
      → verify (hash compare)
      → success / failure screen
```

---

## 3. Safety model (defense in depth)

Four independent layers. A write happens only if **all** of them pass. Each
layer is written so that a bug in one cannot be the only thing standing between
the user and a destroyed disk.

### Layer 1 — Enumeration: USB whole-disks only

Walk `/sys/class/block/*`. A device is a *candidate* only if:

1. **Whole disk**: `/sys/class/block/<dev>/partition` does not exist
   (excludes `sdb1`, `nvme0n1p2`, …).
2. **USB parent chain**: the `device` symlink target (e.g.
   `../usb1/1-1/1-1:1.0/block/sdb`) contains a `usb` path component.
   This is the hard requirement — it means the kernel itself says the disk
   hangs off a USB controller. SATA/NVMe/virtio disks can never match.
3. Cross-check (advisory): `/sys/class/block/<dev>/removable` == `1`.
   If it disagrees, show a prominent warning in the TUI but do not hard-fail
   (some USB enclosures report 0).

Everything else is filtered out before it is ever shown to the user.

### Layer 2 — Exclusion set (belt and braces)

Even if a device somehow passed Layer 1, it is hard-excluded (with a visible
reason in the TUI) if:

- **Root disk**: the whole disk backing the `/` mount. Resolved by
  `statvfs("/")` → major:minor → `/sys/dev/block/<maj>:<min>` → walk up to the
  disk. (Covers running-from-a-live-USB: a live system booted from a USB stick
  is excluded by this.)
- **Any mounted disk**: parse `/proc/self/mounts`, resolve every device to its
  whole disk, exclude all of them. (Covers a USB stick with a data partition
  currently mounted — the exact "I had my photos on there" scenario.)
- **Swap disk**: any disk with a partition listed in `/proc/swaps`.
- **Virtual devices**: `loop*`, `ram*`, `zram*`, `dm-*` (redundant with Layer 1,
  cheap to keep).

The exclusion set is recomputed at **two** moments: when the drive list is
rendered, and again immediately before the write opens the device.

### Layer 3 — Identity pinning

The `/dev/sdX` name can change between selection and write (hotplug reshuffles
letters). So the confirmed target is pinned by:

- its **sysfs path** (stable per physical device),
- its **serial** (`/sys/block/<dev>/device/serial`, when present),
- its **size in bytes**.

At write time the tool re-resolves the device and requires all three to match
the confirmed values. If the user yanked the stick, or a different stick took
the name, the write is refused.

### Layer 4 — Explicit human confirmation

- The summary screen shows: ISO path + size, target disk name, model, size,
  and the line **"ALL DATA ON THIS DRIVE WILL BE ERASED"**.
- The user must **type the exact device name** (e.g. `sdb`) into a prompt.
  No Enter-only, no "Y" shortcut, no timeout that auto-confirms.
- Confirmation is bound to the pinned identity from Layer 3 — confirming
  `sdb` when the pinned device is no longer `sdb` fails.

### Additional invariants

- Target size < ISO size → refuse (shown at selection time).
- Target size > ISO size → allowed (same as Balena Etcher: only the image
  region is written, the rest is left untouched).
- The write opens the device **read-only first** to re-read size/serial, then
  re-opens `O_WRONLY`. No `O_DIRECT` (alignment pitfalls); plain buffered
  writes with a 1 MiB buffer are fast enough on USB.
- After the last byte: `fsync()`, then `BLKFLSBUF` ioctl, then `sync()` — the
  same flush sequence Etcher uses — before declaring success.

---

## 4. Write + verify pipeline

```
open ISO (read)          → stat size, hash full ISO (sha256) in background
open /dev/<disk> (write) → 1 MiB read/write loop, emit progress events
fsync, BLKFLSBUF, sync
open /dev/<disk> (read)  → hash first <iso_size> bytes
compare sha256           → success screen (or failure + advice)
```

Progress: bytes done/total, MB/s, ETA, percentage bar. ISO hashing starts
while the user is confirming, so verification is not on the critical path.

**ISO validation** (`iso.rs`):
- exists, regular file, size > 0
- optional but cheap: ISO 9660 Primary Volume Descriptor signature `CD001`
  at offset `2048 * 16 + 1`. Warn (not fail) if absent, so weird-but-valid
  images (some vendor tools) still work.

**Interruption**: `Ctrl-C` aborts the write cleanly (close fd, flush). A
partially written drive is simply re-flashed from scratch — no resume state to
corrupt.

---

## 5. Privileges

Writing to `/dev/<disk>` needs root (or the `disk` group with permissive udev
rules). On start, the tool tries to open a candidate device read-only; if that
fails with `EPERM`/`EACCES` it offers to **re-exec itself under `sudo`**
(Balena Etcher does the same). It never writes as root to anything that failed
the safety layers — the layers run identically under sudo.

---

## 6. TUI sketch

```
┌ USB Etcher ───────────────────────────────────────────────┐
│ 1. Image      ubuntu-24.04-live-server-amd64.iso (4.5 GiB)│
│ 2. Drive      sdb — SanDisk Ultra 3.0 (15.6 GiB)          │
│ 3. Confirm    ─────────────────────────────────────────── │
│                                                           │
│  ALL DATA ON sdb WILL BE ERASED.                          │
│  Type the drive name to confirm: [sdb▌]                   │
│                                                           │
│  [Enter] flash        [Esc] back                          │
└───────────────────────────────────────────────────────────┘
```

Drive list (step 2) shows only Layer-1 candidates; excluded devices are listed
greyed out **with their exclusion reason** ("in use: / mounted", "not USB",
"this is your system disk") so the user can see *why* something is missing —
transparency is part of the safety story.

---

## 7. Testing strategy

Safety logic must be testable without hardware, so `probe.rs`/`guards.rs` take
a **sysfs root path** (default `/sys`) and a **mounts source** (default
`/proc/self/mounts`) as parameters.

- **Unit tests** — fixture sysfs trees on disk:
  - USB disk under `usb1/1-1/...` → candidate.
  - SATA `sda`, `nvme0n1` → rejected (no usb component).
  - `sdb1` partition → rejected (not whole disk).
  - USB disk that is the root disk → excluded with reason.
  - USB disk with a mounted partition → excluded with reason.
  - USB disk with swap partition → excluded.
  - `removable` flag mismatch → candidate with warning.
- **Unit tests** — mounts parsing (btrfs subvols, tmpfs/proc noise, dm devices
  resolving to underlying disks).
- **Integration test** — the write/verify pipeline against a **regular file**
  target (the target is a `File`; a file stands in for the block device):
  create a 10 MiB "ISO", flash it into a 20 MiB file, assert bytes match,
  assert the tail beyond the image is untouched, assert the hash verify
  passes and that a corrupted target fails verification.
- **Manual checklist** (real hardware, documented in README):
  - flash a real stick, boot it,
  - plug a stick with a mounted data partition → confirm it's excluded,
  - run the tool from a live USB session → confirm the boot stick is excluded,
  - yank the stick at the confirm screen → write refused,
  - non-root run → sudo re-exec path.

---

## 8. Milestones

| # | Milestone | Done when |
|---|-----------|-----------|
| 1 | Skeleton + `probe` | `etcher list` prints USB disks with size/vendor/model from sysfs |
| 2 | `guards` + identity | exclusion unit tests green (fixture trees) |
| 3 | Write pipeline | file-target integration test green (write, flush, hash-verify) |
| 4 | ISO validation | signature/size checks + tests |
| 5 | TUI wizard + confirmation | full flow usable in terminal |
| 6 | Privilege re-exec, Ctrl-C handling, error messages | manual checklist passes |
| 7 | Polish: progress bar, excluded-device reasons, README | first real stick flashed and booted |

Milestones 1–4 are hardware-free and independently testable; hardware is only
needed at 6–7.

---

## 9. Risks / open questions

- **USB enclosures reporting `removable=0`** — handled as warning, not failure
  (Layer 1's sysfs-parent check is the real gate).
- **Device name reshuffles** — handled by Layer 3 identity pinning.
- **A USB disk that is also a RAID/LVM member** — not covered by the current
  layers; low risk (a live-USB target is a whole disk, and mounted-member
  cases are caught by Layer 2). If it bites, add a `/sys/block/<dev>/slaves`
  check to Layer 2.
- **Write speed on slow USB 2.0 sticks** — expected ~10–20 MB/s; progress bar
  and ETA make this tolerable, nothing to fix.
- **Open question**: type-to-confirm uses the short name (`sdb`). If a user
  has two identical sticks, the identity pinning (Layer 3) still protects
  them — the typed name only gates intent, the pin gates the target.
