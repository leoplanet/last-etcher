//! egui front end: 3-step wizard (pick ISO → pick drive → type-to-confirm),
//! progress bar, hash-verify result. The safety core is shared with the CLI.

use crate::{confirm, guards, icons, identity, iso, probe, theme, verify, write};
use probe::{Disk, Sysfs};
use theme::colors;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

#[derive(PartialEq, Clone, Copy, Debug)]
enum Step {
    PickIso,
    PickDrive,
    Confirm,
    Flashing,
    Verifying,
    Done,
}

pub struct EtcherApp {
    step: Step,
    styled: bool,
    iso_input: String,
    iso: Option<iso::IsoInfo>,
    iso_msg: Option<String>,
    iso_hash: Option<thread::JoinHandle<String>>,
    iso_hash_rx: Option<Receiver<u64>>,
    hash_done: u64,
    iso_hash_value: Option<String>,
    disks: Vec<Disk>,
    guards: guards::Guards,
    selected: Option<usize>,
    drive_msg: Option<String>,
    confirm_input: String,
    confirm_msg: Option<String>,
    perm_error: Option<String>,
    root_password: String,
    child: Option<std::process::Child>,
    child_rx: Option<Receiver<String>>,
    flash_log: Vec<String>,
    done_at: Option<Instant>,
    identity: Option<identity::Identity>,
    browse_rx: Option<Receiver<Option<std::path::PathBuf>>>,
    browse_thread: Option<thread::JoinHandle<()>>,
    flash_rx: Option<Receiver<Result<write::Progress, String>>>,
    speed_ema: Option<f64>,

    flash_thread: Option<thread::JoinHandle<Result<String, String>>>,
    cancel: Option<std::sync::Arc<AtomicBool>>,
    verify_rx: Option<Receiver<Result<write::Progress, String>>>,
    verify_thread: Option<std::thread::JoinHandle<Result<(), String>>>,
    progress: write::Progress,
    started: Instant,
    prev: (u64, Instant),
    done: Option<(bool, String)>,
}

impl EtcherApp {
    fn new(_cc: &eframe::CreationContext) -> Self {
        Self::build()
    }

    /// Construct without a creation context (headless tests).
    #[doc(hidden)]
    pub fn new_headless() -> Self {
        Self::build()
    }

    fn build() -> Self {
        let sys = Sysfs::default();
        let guards = guards::Guards::new(sys.clone());
        let disks = sys.whole_disks();
        Self {
            step: Step::PickIso,
            styled: false,
            iso_input: String::new(),
            iso: None,
            iso_msg: None,
            iso_hash: None,
            iso_hash_rx: None,
            hash_done: 0,
            iso_hash_value: None,
            disks,
            guards,
            selected: None,
            drive_msg: None,
            confirm_input: String::new(),
            confirm_msg: None,
            perm_error: None,
            root_password: String::new(),
            child: None,
            child_rx: None,
            flash_log: Vec::new(),
            done_at: None,
            identity: None,
            browse_rx: None,
            browse_thread: None,
            flash_rx: None,
            speed_ema: None,
            flash_thread: None,
            cancel: None,
            verify_rx: None,
            verify_thread: None,
            progress: write::Progress { done: 0, total: 0 },
            started: Instant::now(),
            prev: (0, Instant::now()),
            done: None,
        }
    }

    fn selected_disk(&self) -> Option<&Disk> {
        self.selected.and_then(|i| self.disks.get(i))
    }

    /// Smoothed transfer speed (exponential moving average over updates),
    /// so the ETA doesn't flicker between per-chunk samples.
    fn note_progress(&mut self, new_done: u64) {
        let now = Instant::now();
        let old = self.progress.done;
        if new_done > old {
            // Clamp dt: after a UI stall, several updates arrive in one frame
            // and an unclamped dt would produce absurd speeds (ETA = 0s).
            let dt = now.duration_since(self.prev.1).as_secs_f64().max(0.05);
            // No USB stick is faster than this; caps burst artifacts.
            let inst = ((new_done - old) as f64 / dt).min(3.0 * 1024.0 * 1024.0 * 1024.0);
            self.speed_ema = Some(match self.speed_ema {
                Some(s) => 0.8 * s + 0.2 * inst,
                None => inst,
            });
            self.prev = (new_done, now);
        }
        self.progress.done = new_done;
    }

    fn speed(&self) -> f64 {
        self.speed_ema.unwrap_or(0.0)
    }

    /// True once the ISO hash has completed (value stored, or thread finished).
    fn hash_is_done(&self) -> bool {
        self.iso_hash_value.is_some()
            || self
                .iso_hash
                .as_ref()
                .map(|h| h.is_finished())
                .unwrap_or(false)
    }

    fn pick_iso(&mut self) {
        let path = std::path::PathBuf::from(self.iso_input.trim());
        match iso::validate(&path) {
            Ok(info) => {
                let p = info.path.clone();
                let (htx, hrx) = mpsc::channel::<u64>();
                self.hash_done = 0;
                self.iso_hash_rx = Some(hrx);
                self.iso_hash = Some(thread::spawn(move || {
                    let f = std::fs::File::open(&p).expect("iso disappeared");
                    verify::hash_file_p(&f, None, |done| { let _ = htx.send(done); })
                        .expect("hash failed")
                }));
                self.iso = Some(info);
                self.iso_msg = None;
                let sys = Sysfs::default();
                self.disks = sys.whole_disks();
                self.guards = guards::Guards::new(sys);
                self.selected = None;
                self.step = Step::PickDrive;
            }
            Err(e) => self.iso_msg = Some(format!("{e:#}")),
        }
    }

    fn start_flash(&mut self) {
        let (Some(iso), Some(id), Some(disk)) =
            (self.iso.clone(), self.identity.clone(), self.selected_disk().cloned())
        else {
            return;
        };
        if iso.size > disk.size {
            self.confirm_msg = Some("ISO is larger than the drive".into());
            return;
        }
        // The ISO hash (started at selection time) is joined inside the flash
        // thread so the UI never blocks on it.
        let hash_handle = self.iso_hash.take();
        let iso_path = iso.path.clone();
        let iso_size = iso.size;
        let name = id.name.clone();
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let cancel2 = cancel.clone();
        self.flash_thread = Some(thread::spawn(move || -> Result<String, String> {
            let iso_hash = hash_handle
                .and_then(|h| h.join().ok())
                .unwrap_or_default();
            let sys = Sysfs::default();
            let fresh = sys.disk(&name);
            id.verify(fresh.as_ref())?;
            let src = std::fs::File::open(&iso_path).map_err(|e| e.to_string())?;
            let dst = std::fs::OpenOptions::new()
                .write(true)
                .open(format!("/dev/{name}"))
                .map_err(|e| match e.kind() {
                    std::io::ErrorKind::PermissionDenied => {
                        let exe = std::env::current_exe().unwrap_or_default();
                        format!(
                            "permission denied — close this app and run:\n  sudo {} flash {} {name}",
                            exe.display(),
                            iso_path.display()
                        )
                    }
                    _ => e.to_string(),
                })?;
            write::flash(&src, &dst, iso_size, |p| {
                if cancel2.load(Ordering::Relaxed) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "aborted by user",
                    ));
                }
                let _ = tx.send(Ok(*p));
                Ok(())
            })
            .map_err(|e| e.to_string())?;
            Ok(iso_hash)
        }));
        self.cancel = Some(cancel);
        self.flash_rx = Some(rx);
        self.progress = write::Progress { done: 0, total: iso_size };
        self.started = Instant::now();
        self.prev = (0, self.started);
        self.step = Step::Flashing;
    }

    fn start_verify(&mut self) {
        let (Some(iso), Some(id), Some(iso_hash)) =
            (self.iso.clone(), self.identity.clone(), self.iso_hash_value.clone())
        else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.verify_thread = Some(thread::spawn(move || -> Result<(), String> {
            let f = std::fs::File::open(format!("/dev/{}", id.name)).map_err(|e| e.to_string())?;
            let ok = verify::verify(&f, iso.size, &iso_hash, |done| {
                let _ = tx.send(Ok(write::Progress { done, total: iso.size }));
            })
            .map_err(|e| e.to_string())?;
            if !ok {
                return Err("verification FAILED — drive does not match the ISO".into());
            }
            Ok(())
        }));
        self.verify_rx = Some(rx);
        self.progress = write::Progress { done: 0, total: iso.size };
        self.speed_ema = None;
        self.prev = (0, Instant::now());
        self.step = Step::Verifying;
    }

    /// Drain flash/verify channels without blocking the UI thread.
    /// Poll channels; returns true if visible state changed (→ repaint).
    fn poll(&mut self) -> bool {
        let mut changed = false;
        // Hash progress (from the ISO hashing thread).
        if let Some(rx) = self.iso_hash_rx.as_ref() {
            let mut last: Option<u64> = None;
            while let Ok(b) = rx.try_recv() {
                last = Some(b);
            }
            if let Some(b) = last {
                self.hash_done = b;
                changed = true;
            }
        }
        // Hash thread finished: snap the bar to 100%.
        if self
            .iso_hash
            .as_ref()
            .map(|h| h.is_finished())
            .unwrap_or(false)
        {
            if let Some(size) = self.iso.as_ref().map(|i| i.size) {
                if self.hash_done < size {
                    self.hash_done = size;
                    self.iso_hash_rx = None;
                    changed = true;
                }
            }
        }
        if let Some(rx) = self.child_rx.as_ref() {
            let mut pending = Vec::new();
            while let Ok(line) = rx.try_recv() {
                pending.push(line);
            }
            for line in pending {
                let pct = parse_pct(&line);
                if line == "Writing…" {
                    // Speed baseline: measure from the start of the write, not
                    // from app start (the ISO hash takes minutes).
                    self.prev = (self.progress.done, Instant::now());
                }
                if line == "Verifying…" {
                    // The CLI (sudo path) does write + verify in one process —
                    // mirror the phase change so the stepper and heading move
                    // to Verify Drive, and restart the bar/ETA for the new phase.
                    self.step = Step::Verifying;
                    self.progress.done = 0;
                    self.speed_ema = None;
                    self.prev = (0, Instant::now());
                }
                if line.starts_with('…') {
                    // In-progress line: replace the previous partial.
                    if self
                        .flash_log
                        .last()
                        .map(|l| l.starts_with('…'))
                        .unwrap_or(false)
                    {
                        self.flash_log.pop();
                    }
                }
                self.flash_log.push(line);
                if self.flash_log.len() > 14 {
                    self.flash_log.remove(0);
                }
                // Drive the progress bar from "(NN%)" in the output.
                if let Some(pct) = pct {
                    self.note_progress(self.progress.total * pct / 100);
                }
                changed = true;
            }
        }
        if let Some(child) = self.child.as_mut() {
            if let Ok(Some(status)) = child.try_wait() {
                self.child = None;
                self.child_rx = None;
                self.done = Some(if status.success() {
                    (true, "Success: flash complete and verified. Unplug and boot.".into())
                } else {
                    (
                        false,
                        format!("flash failed (exit {status}) — see log above"),
                    )
                });
                self.go_done();
                changed = true;
            }
        }
        if let Some(rx) = self.browse_rx.as_ref() {
            if let Ok(p) = rx.try_recv() {
                if let Some(p) = p {
                    self.iso_input = p.to_string_lossy().into_owned();
                }
                self.browse_rx = None;
                self.browse_thread = None;
                changed = true;
            }
        }
        if let Some(rx) = self.flash_rx.as_ref() {
            let mut pending = Vec::new();
            let mut disconnected = false;
            loop {
                match rx.try_recv() {
                    Ok(item) => pending.push(item),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                    Err(_) => break,
                }
            }
            for item in pending {
                match item {
                    Ok(p) => {
                        self.note_progress(p.done);
                        self.progress.total = p.total;
                        changed = true;
                    }
                    Err(e) => {
                        self.done = Some((false, e));
                        self.go_done();
                        changed = true;
                        break;
                    }
                }
            }
            if disconnected && self.step == Step::Flashing {
                // Thread finished: join and take its result.
                let res = self
                    .flash_thread
                    .take()
                    .map(|h| match h.join() {
                        Ok(r) => r,
                        Err(payload) => Err(payload
                            .downcast_ref::<String>()
                            .cloned()
                            .unwrap_or_else(|| "write thread panicked".to_string())),
                    })
                    .unwrap_or_else(|| Err("write thread died".into()));
                match res {
                    Ok(hash) => {
                        self.iso_hash_value = Some(hash);
                        self.start_verify();
                    }
                    Err(e) => {
                        self.done = Some((false, e));
                        self.go_done();
                        changed = true;
                    }
                }
            }
        }
        if let Some(rx) = self.verify_rx.as_ref() {
            let mut pending = Vec::new();
            let mut disconnected = false;
            loop {
                match rx.try_recv() {
                    Ok(item) => pending.push(item),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                    Err(_) => break,
                }
            }
            for item in pending {
                match item {
                    Ok(p) => {
                        self.note_progress(p.done);
                        self.progress.total = p.total;
                        changed = true;
                    }
                    Err(e) => {
                        self.done = Some((false, e));
                        self.go_done();
                        changed = true;
                        break;
                    }
                }
            }
            if disconnected && self.step == Step::Verifying {
                let res = self
                    .verify_thread
                    .take()
                    .map(|h| match h.join() {
                        Ok(r) => r,
                        Err(payload) => Err(payload
                            .downcast_ref::<String>()
                            .cloned()
                            .unwrap_or_else(|| "verify thread panicked".to_string())),
                    })
                    .unwrap_or_else(|| Err("verify thread died".into()));
                let name = self.identity.as_ref().map(|i| i.name.clone()).unwrap_or_default();
                self.done = Some(match res {
                    Ok(()) => (true, format!("Success: {name} is ready. Unplug and boot.")),
                    Err(e) => (false, e),
                });
                self.go_done();
                changed = true;
            }
        }
        changed
    }
}

impl eframe::App for EtcherApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.render(ctx);
    }
}

impl EtcherApp {
    /// Render one frame. Split out from [`eframe::App::update`] so headless
    /// tests can drive it without a real [`eframe::Frame`].
    pub fn render(&mut self, ctx: &egui::Context) {
        if !self.styled {
            self.styled = true;
            theme::apply(ctx);
        }
        // Dropping a file anywhere in the window selects it as the image.
        if self.step == Step::PickIso {
            if let Some(path) = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone())) {
                self.iso_input = path.to_string_lossy().into_owned();
                self.pick_iso();
            }
        }
        let changed = self.poll();

        // Header: title + stepper, pinned to the top.
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::default()
                    .fill(colors::WINDOW)
                    .inner_margin(egui::Margin::symmetric(20.0, 10.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("LAST ETCHER").strong().size(theme::type_scale::TITLE));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new("by Leo Choudhary")
                                .weak()
                                .size(theme::type_scale::BADGE),
                        );
                    });
                });
                ui.add_space(6.0);
                self.draw_stepper(ui);
            });

        // Nav buttons, pinned to the bottom of the window.
        let done_ok = self.done.as_ref().map(|(ok, _)| *ok).unwrap_or(false);
        let (next_label, next_enabled, has_back, show_cancel, show_close, show_try_again) =
            match self.step {
                Step::PickIso => ("Continue →", !self.iso_input.trim().is_empty(), false, false, false, false),
                Step::PickDrive => ("Continue →", self.selected.is_some(), true, false, false, false),
                Step::Confirm => (
                    if self.perm_error.is_some() {
                        "Authenticate & FLASH"
                    } else {
                        "FLASH"
                    },
                    self.selected.is_some()
                        && (self.perm_error.is_none() || !self.root_password.is_empty()),
                    true,
                    false,
                    false,
                    false,
                ),
                Step::Flashing | Step::Verifying => ("", false, false, true, false, false),
                Step::Done => ("", false, false, false, true, !done_ok),
            };
        if !next_label.is_empty() || show_cancel || show_close {
            egui::TopBottomPanel::bottom("nav")
                .frame(
                    egui::Frame::default()
                        .fill(colors::WINDOW)
                        .inner_margin(egui::Margin::symmetric(20.0, 12.0)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if show_cancel {
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                egui::RichText::new("Cancel").strong().color(egui::Color32::WHITE),
                                            )
                                            .min_size(egui::vec2(120.0, 36.0))
                                            .rounding(theme::RADIUS_INPUT)
                                            .fill(colors::DANGER_DEEP),
                                        )
                                        .clicked()
                                    {
                                        self.cancel_operation();
                                    }
                                    return;
                                }
                                if show_close {
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                egui::RichText::new("Close").strong().color(egui::Color32::WHITE),
                                            )
                                            .min_size(egui::vec2(120.0, 36.0))
                                            .rounding(theme::RADIUS_INPUT)
                                            .fill(colors::ACCENT),
                                        )
                                        .clicked()
                                    {
                                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                                    }
                                    if show_try_again {
                                        ui.add_space(8.0);
                                        if ui
                                            .add(
                                                egui::Button::new("Try again")
                                                    .min_size(egui::vec2(120.0, 36.0))
                                                    .rounding(theme::RADIUS_INPUT),
                                            )
                                            .clicked()
                                        {
                                            self.confirm_input.clear();
                                            self.confirm_msg = None;
                                            self.flash_log.clear();
                                            self.done = None;
                                            self.step = Step::Confirm;
                                        }
                                    }
                                    return;
                                }
                                let flash = self.step == Step::Confirm;
                                let btn = egui::Button::new(
                                    egui::RichText::new(next_label).strong().color(egui::Color32::WHITE),
                                )
                                .min_size(egui::vec2(if flash { 190.0 } else { 120.0 }, 36.0))
                                .rounding(theme::RADIUS_INPUT)
                                .fill(if flash { colors::DANGER_DEEP } else { colors::ACCENT });
                                if ui.add_enabled(next_enabled, btn).clicked() {
                                    self.on_next();
                                }
                                if has_back {
                                    ui.add_space(8.0);
                                    if ui
                                        .add(
                                            egui::Button::new("Back")
                                                .min_size(egui::vec2(90.0, 36.0))
                                                .rounding(theme::RADIUS_INPUT),
                                        )
                                        .clicked()
                                    {
                                        self.on_back();
                                    }
                                }
                            },
                        );
                    });
                });
        }

        // Content: fills whatever space the window manager gives us, with
        // margins and vertically centered.
        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(colors::WINDOW)
                    .inner_margin(egui::Margin::same(20.0)),
            )
            .show(ctx, |ui| {
                self.draw_step(ui);
            });
        if changed {
            ctx.request_repaint();
        }
        // Keep the event loop awake during long operations. When the window is
        // unfocused/occluded the compositor stops delivering frame events, so
        // without this timer the loop blocks and can't answer the WM's ping
        // ("Application Not Responding") — and the screen goes stale.
        if matches!(self.step, Step::Flashing | Step::Verifying) {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }
}

impl EtcherApp {
    fn draw_stepper(&self, ui: &mut egui::Ui) {
        let labels = ["Select Image", "Select Drive", "Hash Image", "Flash Drive", "Verify Drive"];
        let hash_done = self.hash_is_done();
        let hashing = !hash_done && self.iso_hash.is_some();
        // Per-step (done, active, spinning) state.
        let step_state: [(bool, bool, bool); 5] = match self.step {
            Step::PickIso => [(false, true, false), (false, false, false), (false, false, hashing), (false, false, false), (false, false, false)],
            Step::PickDrive => [(true, false, false), (false, true, false), (false, false, hashing), (false, false, false), (false, false, false)],
            Step::Confirm => [(true, false, false), (true, false, false), (hash_done, hashing, hashing), (false, false, false), (false, false, false)],
            // While the flash thread waits on the hash, we're still hashing.
            Step::Flashing if !hash_done => [(true, false, false), (true, false, false), (false, true, hashing), (false, false, false), (false, false, false)],
            Step::Flashing => [(true, false, false), (true, false, false), (true, false, false), (false, true, false), (false, false, false)],
            Step::Verifying => [(true, false, false), (true, false, false), (true, false, false), (true, false, false), (false, true, false)],
            Step::Done => [(true, false, false), (true, false, false), (true, false, false), (true, false, false), (true, false, false)],
        };
        let total_w = ui.available_width();
        let circle = 24.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(total_w, 46.0), egui::Sense::hover());
        let p = ui.painter();
        let cy = rect.min.y + circle / 2.0;
        let col_w = total_w / labels.len() as f32;
        let t = self.started.elapsed().as_secs_f32();
        for (i, label) in labels.iter().enumerate() {
            let n = (i + 1) as u32;
            let (done, active, spinning) = step_state[i];
            // Circle centered in each equal column.
            let cx = rect.min.x + (i as f32 + 0.5) * col_w;
            let center = egui::pos2(cx, cy);
            // Connector from this circle to the next.
            if i + 1 < labels.len() {
                let next_cx = rect.min.x + (i as f32 + 1.5) * col_w;
                let from = center + egui::vec2(circle / 2.0 + 6.0, 0.0);
                let to = egui::pos2(next_cx, cy) - egui::vec2(circle / 2.0 + 6.0, 0.0);
                let col = if done { colors::ACCENT } else { colors::BORDER };
                p.line_segment([from, to], egui::Stroke::new(2.0_f32, col));
            }
            if done {
                p.circle_filled(center, circle / 2.0, colors::ACCENT);
                icons::check(
                    p,
                    egui::Rect::from_center_size(center, egui::vec2(12.0, 12.0)),
                    egui::Color32::WHITE,
                    1.0,
                );
            } else if spinning {
                p.circle(center, circle / 2.0 - 1.0, egui::Color32::TRANSPARENT, egui::Stroke::new(2.0_f32, colors::ACCENT));
                icons::spinner(p, center, circle / 2.0 - 6.0, colors::ACCENT, t % 1.0);
            } else if active {
                p.circle(center, circle / 2.0 - 1.0, egui::Color32::TRANSPARENT, egui::Stroke::new(2.0_f32, colors::ACCENT));
                p.text(
                    center,
                    egui::Align2::CENTER_CENTER,
                    n.to_string(),
                    theme::font(theme::type_scale::LABEL),
                    colors::ACCENT,
                );
            } else {
                p.circle(center, circle / 2.0 - 1.0, egui::Color32::TRANSPARENT, egui::Stroke::new(1.5_f32, colors::BORDER));
                p.text(
                    center,
                    egui::Align2::CENTER_CENTER,
                    n.to_string(),
                    theme::font(theme::type_scale::LABEL),
                    colors::TEXT_3,
                );
            }
            // Label centered below the circle.
            let label_color = if done || active || spinning {
                colors::TEXT
            } else {
                colors::TEXT_3
            };
            p.text(
                egui::pos2(cx, rect.min.y + circle + 12.0),
                egui::Align2::CENTER_CENTER,
                label,
                theme::font(theme::type_scale::CAPTION),
                label_color,
            );
        }
    }

    /// A full-width card that is only as tall as its content.
    ///
    /// A bare `egui::Frame` sizes to its content, so it comes out narrow and
    /// left-aligned. Forcing the content to full width instead makes the Frame
    /// fill the parent's *height* (the tiling-WM window is tall, so the card
    /// balloons to ~900px). So we draw the card ourselves: lay the content out
    /// in a child (inset by the pad, full inner width) to measure its height,
    /// then allocate a full-width, content-height rect at the cursor and paint
    /// the rounded background behind the already-rendered content.
    fn card(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
        const MARGIN: f32 = 14.0;
        const STROKE: f32 = 1.0;
        let pad = MARGIN + STROKE;
        // Reserve a paint slot *before* the content so the background is drawn
        // underneath it (painting it after would cover the content).
        let bg_id = ui.painter().add(egui::Shape::Noop);
        let avail = ui.available_size();
        let inner_w = (avail.x - 2.0 * pad).max(0.0);
        let origin = ui.cursor().min;
        let child_rect = egui::Rect::from_min_size(
            origin + egui::vec2(pad, pad),
            egui::vec2(inner_w, avail.y),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(child_rect)
                .layout(egui::Layout::top_down(egui::Align::LEFT)),
        );
        content(&mut child);
        let content_h = child.min_size().y;
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(avail.x, content_h + 2.0 * pad),
            egui::Sense::hover(),
        );
        ui.painter().set(
            bg_id,
            egui::Shape::Rect(egui::epaint::RectShape {
                rect,
                rounding: egui::Rounding::same(theme::RADIUS_CARD),
                fill: colors::RAISED,
                stroke: egui::Stroke::new(STROKE, colors::BORDER),
                blur_width: 0.0,
                fill_texture_id: egui::TextureId::default(),
                uv: egui::Rect::NOTHING,
            }),
        );
    }

    fn on_next(&mut self) {
        match self.step {
            Step::PickIso => self.pick_iso(),
            Step::PickDrive => {
                if let Some(d) = self.selected_disk().cloned() {
                    self.identity = Some(identity::Identity::from(&d));
                    self.confirm_input.clear();
                    self.confirm_msg = None;
                    self.perm_error = Self::check_permission(&d.name);
                    self.step = Step::Confirm;
                }
            }
            Step::Confirm => {
                if let Some(id) = &self.identity {
                    if !confirm::matches(&self.confirm_input, &id.name) {
                        self.confirm_msg =
                            Some(format!("type the exact drive name \"{}\"", id.name));
                        return;
                    }
                    if self.perm_error.is_some() {
                        if let (Some(iso), Some(d)) = (self.iso.clone(), self.selected_disk().cloned()) {
                            let password = self.root_password.clone();
                            self.start_sudo_flash(&iso, &d.name, &password);
                        }
                    } else {
                        self.start_flash();
                    }
                }
            }
            _ => {}
        }
    }

    /// Run the CLI flash subcommand under `sudo -S` (password via stdin pipe),
    /// streaming its output into the progress screen.
    fn start_sudo_flash(&mut self, iso: &iso::IsoInfo, dev: &str, password: &str) {
        let exe = std::env::current_exe().unwrap_or_default();
        let mut cmd = std::process::Command::new("sudo");
        cmd.arg("-S")
            .arg(&exe)
            .arg("flash")
            .arg(&iso.path)
            .arg(dev)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.done = Some((false, format!("failed to start sudo: {e}")));
                self.go_done();
                return;
            }
        };
        if let Some(mut s) = child.stdin.take() {
            use std::io::Write;
            let _ = s.write_all(format!("{password}\n").as_bytes());
            let _ = s.flush();
        }
        let stderr = child.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            use std::io::Read;
            let mut r = std::io::BufReader::new(stderr);
            let mut buf = [0u8; 4096];
            let mut cur = String::new();
            let mut last_partial = String::new();
            loop {
                let n = match r.read(&mut buf) {
                    Ok(n) if n > 0 => n,
                    _ => break,
                };
                cur.push_str(&String::from_utf8_lossy(&buf[..n]));
                while let Some(pos) = cur.find('\n') {
                    let line: String = cur.drain(..=pos).collect();
                    // Progress uses \r; keep only the final segment.
                    let line = line.split('\r').last().unwrap_or("").trim_end();
                    last_partial.clear();
                    if !line.is_empty() {
                        let _ = tx.send(line.to_string());
                    }
                }
                // Flush the in-progress \r line so the UI shows live progress.
                let partial = cur.split('\r').last().unwrap_or("").trim_end().to_string();
                if partial != last_partial && !partial.is_empty() {
                    last_partial = partial.clone();
                    let _ = tx.send(format!("…{partial}"));
                }
                // The CLI never sends a final \n for progress, so keep only
                // the in-progress segment — an unbounded buffer makes this
                // loop O(n²) and progress updates slow down over time.
                if let Some(pos) = cur.rfind('\r') {
                    cur.drain(..=pos);
                }
            }
        });
        self.child = Some(child);
        self.child_rx = Some(rx);
        self.flash_log.clear();
        self.progress = write::Progress { done: 0, total: iso.size };
        self.started = Instant::now();
        self.prev = (0, self.started);
        self.step = Step::Flashing;
    }

    /// Unmount all mountpoints of a USB disk (user-level umount), then refresh.
    fn try_unmount(&mut self, disk: &Disk) {
        let points = self.guards.mount_points_of(&disk.name);
        let mut errs = Vec::new();
        for p in &points {
            let out = std::process::Command::new("umount").arg(p).output();
            match out {
                Ok(o) if o.status.success() => {}
                Ok(o) => errs.push(format!(
                    "umount {p}: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                )),
                Err(e) => errs.push(format!("umount {p}: {e}")),
            }
        }
        // Refresh the drive list and exclusions.
        let sys = Sysfs::default();
        self.disks = sys.whole_disks();
        self.guards = guards::Guards::new(sys);
        self.selected = None;
        self.drive_msg = if errs.is_empty() {
            None
        } else {
            Some(errs.join("; "))
        };
    }

    /// Can the current user open the device? If not, explain how to fix it.
    fn check_permission(name: &str) -> Option<String> {
        match std::fs::OpenOptions::new().read(true).open(format!("/dev/{name}")) {
            Ok(_) => None,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                let user = std::env::var("USER").unwrap_or_default();
                let exe = std::env::current_exe().unwrap_or_default();
                Some(format!(
                    "No write permission on /dev/{name} (not in the `disk` group).\n\
                     One-time fix:  sudo usermod -aG disk {user}   (then log out and in)\n\
                     Or flash now:  sudo {} flash <iso> {name}",
                    exe.display()
                ))
            }
            Err(_) => None,
        }
    }

    fn go_done(&mut self) {
        self.done_at = Some(Instant::now());
        self.step = Step::Done;
    }

    fn on_back(&mut self) {
        self.step = match self.step {
            Step::PickDrive => Step::PickIso,
            Step::Confirm => Step::PickDrive,
            _ => self.step,
        };
    }

    /// Cancel an in-flight flash/verify: signal the writer and kill the child.
    fn cancel_operation(&mut self) {
        if let Some(c) = &self.cancel {
            c.store(true, Ordering::Relaxed);
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }
    }

    /// Which screen to draw.
    fn draw_step(&mut self, ui: &mut egui::Ui) {
        match self.step {
            Step::PickIso => self.ui_pick_iso(ui),
            Step::PickDrive => self.ui_pick_drive(ui),
            Step::Confirm => self.ui_confirm(ui),
            Step::Flashing => self.ui_progress(ui, "Writing"),
            Step::Verifying => self.ui_progress(ui, "Verifying"),
            Step::Done => self.ui_done(ui),
        }
    }

    fn ui_pick_iso(&mut self, ui: &mut egui::Ui) {
        Self::card(ui, |ui| {
            ui.label(egui::RichText::new("Choose the image to flash").strong().size(theme::type_scale::HEADING));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.iso_input)
                        .hint_text("/path/to/image.iso  —  or drop a file here")
                        .desired_width(400.0),
                );
                if ui
                    .add(
                        egui::Button::new("Browse…")
                            .min_size(egui::vec2(90.0, 34.0))
                            .rounding(theme::RADIUS_INPUT),
                    )
                    .clicked()
                {
                    // Native dialog runs off the UI thread so it can't freeze the app.
                    if self.browse_rx.is_none() {
                        let (tx, rx) = mpsc::channel();
                        self.browse_rx = Some(rx);
                        self.browse_thread = Some(thread::spawn(move || {
                            let p = rfd::FileDialog::new().pick_file();
                            let _ = tx.send(p);
                        }));
                    }
                }
            });
            ui.add_space(14.0);
            if let Some(iso) = &self.iso {
                // Selected-file chip.
                egui::Frame::default()
                    .fill(colors::SURFACE)
                    .rounding(theme::RADIUS_CHIP)
                    .stroke(egui::Stroke::new(1.0_f32, colors::BORDER))
                    .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (icon_rect, _) =
                                ui.allocate_exact_size(egui::vec2(18.0, 22.0), egui::Sense::hover());
                            icons::file(ui.painter(), icon_rect, colors::TEXT_2);
                            ui.add_space(10.0);
                            ui.vertical_centered(|ui| {
                                ui.label(egui::RichText::new(iso.path.display().to_string()).strong());
                                ui.label(egui::RichText::new(human_size(iso.size)).weak().size(theme::type_scale::LABEL));
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if iso.is_iso9660 {
                                    Self::badge(ui, "ISO 9660", colors::SUCCESS);
                                }
                            });
                        });
                    });
                if !iso.is_iso9660 {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("warning: no ISO-9660 signature found")
                            .color(colors::WARN)
                            .size(theme::type_scale::LABEL),
                    );
                }
                // Hashing indicator: live progress bar.
                if self
                    .iso_hash
                    .as_ref()
                    .map(|h| !h.is_finished())
                    .unwrap_or(false)
                {
                    let total = iso.size.max(1) as f32;
                    let frac = (self.hash_done as f32 / total).clamp(0.0, 1.0);
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let (c, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                        let t = self.started.elapsed().as_secs_f32();
                        icons::spinner(ui.painter(), c.center(), 7.0, colors::ACCENT, t % 1.0);
                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("Hashing image").color(colors::TEXT_2).size(theme::type_scale::LABEL));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.label(egui::RichText::new(format!("{}%", (frac * 100.0) as u32)).color(colors::TEXT_2).size(theme::type_scale::LABEL));
                                });
                            });
                            ui.add_space(6.0);
                            let (bar, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 8.0), egui::Sense::hover());
                            let p = ui.painter();
                            p.rect(bar, 4.0, colors::RAISED, egui::Stroke::NONE);
                            if frac > 0.001 {
                                let fw = (bar.width() * frac).max(8.0);
                                let fill = egui::Rect::from_min_size(bar.min, egui::vec2(fw, 8.0));
                                p.rect(fill, 4.0, colors::ACCENT, egui::Stroke::NONE);
                                icons::glisten(p, fill, t * 60.0 % fw);
                            }
                        });
                    });
                }
            }
            if let Some(msg) = &self.iso_msg {
                ui.add_space(6.0);
                ui.colored_label(colors::DANGER, msg);
            }
        });
    }

    /// Small rounded status pill, in layout flow.
    fn badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
        let text_size = ui
            .painter()
            .layout_no_wrap(text.to_string(), theme::font(theme::type_scale::BADGE), color)
            .size();
        let size = text_size + egui::vec2(14.0, 7.0);
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        let p = ui.painter();
        p.rect(
            rect,
            theme::RADIUS_CHIP,
            theme::tint(color, 38),
            egui::Stroke::new(1.0_f32, theme::tint(color, 128)),
        );
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            theme::font(theme::type_scale::BADGE),
            color,
        );
    }

    fn ui_pick_drive(&mut self, ui: &mut egui::Ui) {
        Self::card(ui, |ui| {
            ui.label(egui::RichText::new("Choose the target USB drive").strong().size(theme::type_scale::BODY));
            ui.add_space(10.0);
            if self.disks.is_empty() {
                ui.colored_label(
                    colors::DANGER,
                    "No block devices found. Plug in a USB drive and restart.",
                );
            }
            let has_candidate = self
                .disks
                .iter()
                .any(|d| d.is_usb && self.guards.exclusion(&d.name).is_none());
            if !self.disks.is_empty() && !has_candidate {
                ui.add_space(4.0);
                ui.colored_label(
                    colors::WARN,
                    "No flashable USB drive. If yours is listed below as mounted, \
                     click Unmount next to it.",
                );
            }
            if let Some(msg) = &self.drive_msg {
                ui.add_space(4.0);
                ui.colored_label(colors::DANGER, msg);
            }
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                let items: Vec<(usize, Disk, Option<guards::Exclusion>)> = self
                    .disks
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(i, d)| {
                        let ex = self.guards.exclusion(&d.name);
                        (i, d, ex)
                    })
                    .collect();
                for (i, d, excluded) in items {
                    let candidate = d.is_usb && excluded.is_none();
                    let selected = self.selected == Some(i) && candidate;
                    let dimmed = !candidate;
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(ui.available_width(), 60.0), egui::Sense::click());
                    let p = ui.painter();
                    let hovered = resp.hovered() && candidate;
                    let fill = if selected {
                        theme::tint(colors::ACCENT, 46)
                    } else if hovered {
                        colors::RAISED_HOVER
                    } else if dimmed {
                        colors::SURFACE
                    } else {
                        colors::RAISED
                    };
                    let stroke = if selected {
                        egui::Stroke::new(1.5_f32, colors::ACCENT)
                    } else {
                        egui::Stroke::new(1.0_f32, colors::BORDER)
                    };
                    p.rect(rect, 10.0, fill, stroke);

                    // Radio indicator (selection affordance).
                    let radio_c = egui::pos2(rect.min.x + 22.0, rect.center().y);
                    if selected {
                        p.circle_filled(radio_c, 8.0, colors::ACCENT);
                        p.circle_filled(radio_c, 3.5, colors::WINDOW);
                    } else {
                        p.circle(radio_c, 7.0, egui::Color32::TRANSPARENT, egui::Stroke::new(1.5_f32, if dimmed { colors::BORDER } else { colors::TEXT_3 }));
                    }

                    // Disk icon.
                    let icon_c = egui::pos2(rect.min.x + 52.0, rect.center().y);
                    icons::disk(
                        p,
                        egui::Rect::from_center_size(icon_c, egui::vec2(22.0, 17.0)),
                        if dimmed { colors::TEXT_3 } else { colors::TEXT_2 },
                    );

                    // Name + model (left block).
                    let text_x = rect.min.x + 76.0;
                    let name_col = if dimmed { colors::TEXT_3 } else { colors::TEXT };
                    p.text(
                        rect.min + egui::vec2(text_x, 13.0),
                        egui::Align2::LEFT_TOP,
                        &d.name,
                        theme::font(theme::type_scale::BODY),
                        name_col,
                    );
                    let model = match (d.vendor.as_deref(), d.model.as_deref()) {
                        (Some(v), Some(m)) if !m.is_empty() => format!("{v} {m}"),
                        (Some(v), _) => v.to_string(),
                        (_, Some(m)) if !m.is_empty() => m.to_string(),
                        _ => String::new(),
                    };
                    p.text(
                        rect.min + egui::vec2(text_x, 34.0),
                        egui::Align2::LEFT_TOP,
                        &model,
                        theme::font(theme::type_scale::CAPTION),
                        if dimmed { colors::TEXT_3 } else { colors::TEXT_2 },
                    );

                    // Size (top-right) + badge (below it), with padding.
                    let size_txt = human_size(d.size);
                    let sw = p.layout_no_wrap(size_txt.clone(), theme::font(theme::type_scale::LABEL), colors::TEXT_2).size();
                    p.text(
                        rect.min + egui::vec2(rect.width() - sw.x - 16.0, 11.0),
                        egui::Align2::RIGHT_TOP,
                        &size_txt,
                        theme::font(theme::type_scale::LABEL),
                        if dimmed { colors::TEXT_3 } else { colors::TEXT_2 },
                    );
                    // Right side: Unmount (mounted) or reason badge (excluded).
                    // Candidates have no button — the whole row is the selection,
                    // shown by the radio + accent border.
                    let show_unmount = excluded == Some(guards::Exclusion::Mounted) && d.is_usb;
                    let btn_rect = egui::Rect::from_center_size(
                        egui::pos2(rect.max.x - 56.0, rect.min.y + 45.0),
                        egui::vec2(84.0, 24.0),
                    );
                    if show_unmount {
                        let disk = d.clone();
                        p.rect(btn_rect, 6.0, colors::RAISED, egui::Stroke::new(1.0_f32, colors::BORDER));
                        p.text(btn_rect.center(), egui::Align2::CENTER_CENTER, "Unmount", theme::font(theme::type_scale::LABEL), colors::TEXT_2);
                        if ui
                            .interact(btn_rect, egui::Id::new(format!("unmount-{i}")), egui::Sense::click())
                            .clicked()
                        {
                            self.try_unmount(&disk);
                        }
                    } else if !candidate {
                        let reason = excluded
                            .map(|e| e.reason().to_string())
                            .unwrap_or_else(|| "not USB".into());
                        let bw = reason.chars().count() as f32 * 6.2 + 16.0;
                        let badge_rect = egui::Rect::from_center_size(
                            egui::pos2(rect.max.x - 16.0 - bw / 2.0, rect.min.y + 45.0),
                            egui::vec2(bw, 18.0),
                        );
                        p.rect(badge_rect, theme::RADIUS_CHIP, theme::tint(colors::TEXT_3, 38), egui::Stroke::new(1.0_f32, theme::tint(colors::TEXT_3, 128)));
                        p.text(badge_rect.center(), egui::Align2::CENTER_CENTER, &reason, theme::font(theme::type_scale::BADGE), colors::TEXT_3);
                    }

                    if candidate {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() && candidate {
                        self.selected = Some(i);
                    }
                    ui.add_space(8.0);
                }
            });
        });
    }

    fn ui_confirm(&mut self, ui: &mut egui::Ui) {
        Self::card(ui, |ui| {
            ui.label(egui::RichText::new("Confirm").strong().size(theme::type_scale::HEADING));
            ui.add_space(12.0);
            // Summary: image → drive.
            ui.columns(2, |cols| {
                for (col, (title, icon, text, sub)) in cols
                    .iter_mut()
                    .zip([
                        (
                            "Image",
                            true,
                            self
                                .iso
                                .as_ref()
                                .map(|i| i.path.display().to_string())
                                .unwrap_or_default(),
                            self.iso.as_ref().map(|i| human_size(i.size)).unwrap_or_default(),
                        ),
                        (
                            "Target",
                            false,
                            self
                                .selected_disk()
                                .map(|d| d.name.clone())
                                .unwrap_or_default(),
                            self
                                .selected_disk()
                                .map(|d| {
                                    format!(
                                        "{} · {}",
                                        describe(d),
                                        human_size(d.size)
                                    )
                                })
                                .unwrap_or_default(),
                        ),
                    ])
                {
                    col.label(egui::RichText::new(title).weak().size(theme::type_scale::LABEL));
                    col.add_space(4.0);
                    col.horizontal(|ui| {
                        let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 20.0), egui::Sense::hover());
                        if icon {
                            icons::file(ui.painter(), icon_rect, colors::TEXT_2);
                        } else {
                            icons::disk(ui.painter(), icon_rect, colors::TEXT_2);
                        }
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new(&text).strong().size(theme::type_scale::LABEL));
                    });
                    col.add_space(2.0);
                    col.label(egui::RichText::new(&sub).weak().size(theme::type_scale::LABEL));
                }
            });
            ui.add_space(14.0);
            // Danger banner.
            if let Some(d) = self.selected_disk() {
                egui::Frame::default()
                    .fill(colors::RAISED)
                    .rounding(8.0)
                    .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(20.0, 18.0), egui::Sense::hover());
                            icons::warn(ui.painter(), icon_rect, colors::WARN);
                            ui.add_space(10.0);
                            ui.label(
                                egui::RichText::new(format!("ALL DATA ON {} WILL BE ERASED", d.name))
                                    .strong()
                                    .color(colors::WARN),
                            );
                        });
                    });
                ui.add_space(14.0);
                // Type-to-confirm with live feedback.
                if let Some(id) = &self.identity {
                    let matched = confirm::matches(&self.confirm_input, &id.name);
                    let border = if self.confirm_input.is_empty() {
                        colors::BORDER
                    } else if matched {
                        colors::SUCCESS
                    } else {
                        colors::DANGER
                    };
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("Type \"{}\" to confirm:", id.name)).size(theme::type_scale::LABEL));
                        // Live border color: recolor the widget frame on the fly.
                        {
                            let s = ui.style_mut();
                            s.visuals.widgets.inactive.bg_fill = colors::SURFACE;
                            s.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.5_f32, border);
                            s.visuals.widgets.active.bg_fill = colors::SURFACE;
                            s.visuals.widgets.active.bg_stroke = egui::Stroke::new(1.5_f32, border);
                        }
                        ui.add(
                            egui::TextEdit::singleline(&mut self.confirm_input)
                                .font(theme::mono(15.0))
                                .frame(true)
                                .text_color(if matched {
                                    colors::SUCCESS
                                } else {
                                    colors::TEXT
                                })
                                .cursor_at_end(true)
                                .desired_width(160.0),
                        );
                    });
                    if matched {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            let (c, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            icons::check(ui.painter(), egui::Rect::from_center_size(c.center(), egui::vec2(14.0, 14.0)), colors::SUCCESS, 1.0);
                            ui.label(egui::RichText::new("match").color(colors::SUCCESS).size(theme::type_scale::LABEL));
                        });
                    }
                }
            }
            if let Some(msg) = &self.confirm_msg {
                ui.add_space(8.0);
                ui.colored_label(colors::DANGER, msg);
            }
            if let Some(msg) = &self.perm_error {
                ui.add_space(10.0);
                egui::Frame::default()
                    .fill(colors::RAISED)
                    .rounding(8.0)
                    .inner_margin(egui::Margin::symmetric(14.0, 12.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (ic, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                            icons::warn(ui.painter(), ic, colors::WARN);
                            ui.add_space(10.0);
                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new("Administrator access needed").strong().size(theme::type_scale::BODY));
                                for line in msg.lines() {
                                    ui.label(egui::RichText::new(line).color(colors::TEXT_2).size(theme::type_scale::LABEL));
                                }
                            });
                        });
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Root password").color(colors::TEXT_2).size(theme::type_scale::LABEL));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.root_password)
                                    .password(true)
                                    .desired_width(220.0),
                            );
                        });
                    });
            }
        });
    }

    fn ui_progress(&mut self, ui: &mut egui::Ui, label: &str) {
        let name = self.identity.as_ref().map(|i| i.name.clone()).unwrap_or_default();
        // While the flash thread waits on the ISO hash, we're still hashing.
        let in_hash = self.step == Step::Flashing && !self.hash_is_done();
        let (total, done) = if in_hash {
            (self.iso.as_ref().map(|i| i.size).unwrap_or(1).max(1), self.hash_done)
        } else {
            (self.progress.total.max(1), self.progress.done)
        };
        Self::card(ui, |ui| {
            let heading = if in_hash {
                "Hashing image".to_string()
            } else {
                format!("{label} to {name}")
            };
            ui.label(egui::RichText::new(heading).strong().size(theme::type_scale::HEADING));
            ui.add_space(16.0);
            // Custom progress bar with sheen.
            let total_f = total as f32;
            let frac = (done as f32 / total_f).clamp(0.0, 1.0);
            let time = self.started.elapsed().as_secs_f32();
            let (bar_rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 10.0), egui::Sense::hover());
            let p = ui.painter();
            p.rect(bar_rect, 5.0, colors::RAISED, egui::Stroke::NONE);
            if frac > 0.001 {
                let fw = (bar_rect.width() * frac).max(10.0);
                let fill_rect = egui::Rect::from_min_size(bar_rect.min, egui::vec2(fw, 10.0));
                p.rect(fill_rect, 5.0, colors::ACCENT, egui::Stroke::NONE);
                // Small soft glisten sweeping within the filled portion.
                icons::glisten(p, fill_rect, time * 60.0 % fw);
            }
            ui.add_space(10.0);
            // Big percentage + stats.
            let speed = self.speed();
            let eta = if speed > 0.0 {
                (self.progress.total - self.progress.done) as f64 / speed
            } else {
                f64::INFINITY
            };
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("{}%", (frac * 100.0) as u32)).strong().size(26.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(format!("{} / {}", human_size(done), human_size(total))).weak().size(theme::type_scale::LABEL));
                    if !in_hash {
                        ui.label(egui::RichText::new(format!(
                            "{:.1} MiB/s  ·  ETA {}",
                            speed / 1048576.0,
                            if eta.is_finite() { format_hms(eta as u64) } else { "—".into() }
                        ))
                        .weak()
                        .size(theme::type_scale::LABEL));
                    }
                });
            });
            ui.add_space(14.0);
            if label == "Writing" {
                ui.label(egui::RichText::new("Do not unplug the drive").weak().size(theme::type_scale::LABEL));
            }
            if !self.flash_log.is_empty() {
                ui.add_space(10.0);
                egui::CollapsingHeader::new(egui::RichText::new("Details").size(theme::type_scale::LABEL).weak())
                    .default_open(false)
                    .show(ui, |ui| {
                        egui::Frame::default()
                            .fill(colors::SURFACE)
                            .rounding(6.0)
                            .inner_margin(egui::Margin::same(8.0))
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                                    for line in &self.flash_log {
                                        ui.label(egui::RichText::new(line).monospace().size(theme::type_scale::BADGE));
                                    }
                                });
                            });
                    });
            }
        });
    }

    fn ui_done(&mut self, ui: &mut egui::Ui) {
        let (ok, msg) = self
            .done
            .as_ref()
            .map(|(ok, m)| (*ok, m.clone()))
            .unwrap_or((false, "unknown error".to_string()));
        let color = if ok { colors::SUCCESS } else { colors::DANGER };
        let t = theme::ease_out(
            self.done_at
                .map(|t| t.elapsed().as_secs_f32())
                .unwrap_or(1.0)
                / 0.6,
        );
        Self::card(ui, |ui| {
            ui.vertical_centered(|ui| {
                // Animated mark.
                let (mark_rect, _) = ui.allocate_exact_size(egui::vec2(72.0, 72.0), egui::Sense::hover());
                let p = ui.painter();
                let c = mark_rect.center();
                p.circle(c, 34.0 * t, egui::Color32::TRANSPARENT, egui::Stroke::new(3.0_f32, color));
                let inner = egui::Rect::from_center_size(c, egui::vec2(34.0, 34.0));
                if ok {
                    icons::check(p, inner, color, t);
                } else {
                    icons::x_mark(p, inner, color, t);
                }
                ui.add_space(16.0);
                ui.label(egui::RichText::new(msg).strong().size(theme::type_scale::HEADING));
                if ok {
                    ui.label(egui::RichText::new("Unplug the drive and boot from it.").weak().size(theme::type_scale::LABEL));
                }
                if !self.flash_log.is_empty() {
                    ui.add_space(12.0);
                    egui::CollapsingHeader::new(egui::RichText::new("Log").size(theme::type_scale::LABEL).weak())
                        .default_open(!ok)
                        .show(ui, |ui| {
                            egui::Frame::default()
                                .fill(colors::SURFACE)
                                .rounding(6.0)
                                .inner_margin(egui::Margin::same(8.0))
                                .show(ui, |ui| {
                                    egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                                        for line in &self.flash_log {
                                            ui.label(egui::RichText::new(line).monospace().size(theme::type_scale::BADGE));
                                        }
                                    });
                                });
                        });
                }
            });
        });
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

/// Extract the last "(NN%)" percentage from a line of CLI output.
fn parse_pct(line: &str) -> Option<u64> {
    let start = line.rfind('(')?;
    let rest = &line[start + 1..];
    let end = rest.find("%)")?;
    rest[..end].parse().ok()
}

fn format_hms(s: u64) -> String {
    if s >= 3600 {
        format!("{}h {:02}m {:02}s", s / 3600, (s % 3600) / 60, s % 60)
    } else if s >= 60 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
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

pub fn run() -> anyhow::Result<()> {
    let icon = image::load_from_memory(include_bytes!("../assets/icon.png"))
        .ok()
        .map(|img| {
            let rgba = img.to_rgba8();
            let (w, h) = (rgba.width() as u32, rgba.height() as u32);
            std::sync::Arc::new(egui::IconData { rgba: rgba.into_raw(), width: w, height: h })
        });
    let mut wgpu_options = egui_wgpu::WgpuConfiguration::default();
    // AutoVsync blocks on compositor frame callbacks, which are never
    // delivered for an occluded/background window — that stalls the whole
    // event loop ("Application Not Responding"). Fifo commits without
    // waiting for the frame ack.
    wgpu_options.present_mode = wgpu::PresentMode::Fifo;
    wgpu_options.desired_maximum_frame_latency = Some(1);
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([720.0, 540.0])
        .with_min_inner_size([560.0, 440.0]);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        // wgpu renderer: its present() does not block on compositor frame
        // callbacks the way the default GL backend's glSwapBuffers does for
        // occluded windows (that block caused the ANR dialog).
        renderer: eframe::Renderer::Wgpu,
        wgpu_options,
        ..Default::default()
    };
    eframe::run_native("LAST ETCHER", options, Box::new(|cc| Ok(Box::new(EtcherApp::new(cc)))))
        .map_err(|e| anyhow::anyhow!("failed to start GUI: {e}"))
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use std::path::PathBuf;

    /// The window size every consistency test renders at — all screens are
    /// judged against the same box so layout regressions are comparable.
    const TEST_WINDOW: egui::Rect = egui::Rect::from_min_max(
        egui::pos2(0.0, 0.0),
        egui::pos2(760.0, 540.0),
    );

    /// Render one frame headlessly (no real window/frame).
    fn render_frame(app: &mut EtcherApp) {
        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            screen_rect: Some(TEST_WINDOW),
            ..Default::default()
        };
        let _ = ctx.run(raw, |ctx| app.render(ctx));
    }

    /// Render a single screen into a fixed-size Ui and report whether the
    /// layout content fits (no overflow). Catches screens that grow taller
    /// than the window — the main source of the "bolted on / bunched" look.
    fn screen_fits(
        app: &mut EtcherApp,
        draw: impl Fn(&mut EtcherApp, &mut egui::Ui),
    ) -> bool {
        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            screen_rect: Some(TEST_WINDOW),
            ..Default::default()
        };
        let mut fits = true;
        let _ = ctx.run(raw, |ctx| {
            let mut ui = egui::Ui::new(
                ctx.clone(),
                egui::LayerId::new(egui::Order::Foreground, egui::Id::new("fit-layer")),

                egui::Id::new("fit-check"),
                egui::UiBuilder::new()
                    .max_rect(TEST_WINDOW)
                    .layout(egui::Layout::top_down(egui::Align::LEFT)),
            );
            draw(app, &mut ui);
            let avail = ui.available_size();
            if avail.x < -1.0 || avail.y < -1.0 {
                fits = false;
            }
        });
        fits
    }

    /// The sudo path runs the CLI (write + verify) as one child process. The
    /// GUI must mirror the CLI's "Verifying…" marker so the stepper and
    /// heading move to Verify Drive. Regression test for the "timeline never
    /// reaches Verify Drive" bug.
    #[test]
    fn sudo_path_transitions_to_verifying() {
        let mut app = EtcherApp::build();
        app.step = Step::Flashing;
        let (tx, rx) = std::sync::mpsc::channel();
        app.child_rx = Some(rx);

        app.poll();
        tx.send("Writing…".to_string()).unwrap();
        app.poll();
        assert_eq!(app.step, Step::Flashing, "Writing… must stay on Flash Drive");

        tx.send("Verifying…".to_string()).unwrap();
        app.poll();
        assert_eq!(app.step, Step::Verifying, "Verifying… must move to Verify Drive");
        assert_eq!(app.progress.done, 0, "verify phase must restart the bar");
    }

    fn test_iso() -> iso::IsoInfo {
        iso::IsoInfo {
            path: PathBuf::from("/tmp/test.iso"),
            size: 1_000_000,
            is_iso9660: true,
        }
    }

    /// Drive the app through every screen and assert none of them panic
    /// during render. This is the guard against layout/theme regressions
    /// (e.g. a missing font style, an assert in a painter call).
    #[test]
    fn renders_every_screen_without_panic() {
        let mut app = EtcherApp::build();

        // 1. PickIso
        render_frame(&mut app);

        // 2. PickDrive
        app.step = Step::PickDrive;
        render_frame(&mut app);

        // 3. Confirm (needs an ISO + a selected drive)
        app.iso = Some(test_iso());
        app.selected = Some(0);
        app.step = Step::Confirm;
        render_frame(&mut app);

        // 4. Flashing — still hashing (flash thread waiting on the hash).
        app.step = Step::Flashing;
        render_frame(&mut app);

        // 4b. Flashing — hash done, writing.
        app.hash_done = 1_000_000;
        app.iso_hash_value = Some("abc".into());
        render_frame(&mut app);

        // 5. Verifying
        app.step = Step::Verifying;
        render_frame(&mut app);

        // 6. Done (success)
        app.done = Some((true, "Success".into()));
        app.done_at = Some(Instant::now());
        app.step = Step::Done;
        render_frame(&mut app);

        // 7. Done (failure)
        app.done = Some((false, "boom".into()));
        render_frame(&mut app);
    }

    /// The live window is tall (the tiling WM stretches it). Render a screen at
    /// the real size and return the content height. A screen that spreads across
    /// the full 926px height (the `horizontal_centered`-in-a-Frame bug) returns
    /// ~900+; a compact, top-aligned screen returns a small value.
    fn live_used_h(app: &mut EtcherApp, draw: impl Fn(&mut EtcherApp, &mut egui::Ui)) -> f32 {
        let live = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(766.0, 926.0));
        let ctx = egui::Context::default();
        let raw = egui::RawInput { screen_rect: Some(live), ..Default::default() };
        let mut used_h = f32::NAN;
        let _ = ctx.run(raw, |ctx| {
            let mut ui = egui::Ui::new(
                ctx.clone(),
                egui::LayerId::new(egui::Order::Foreground, egui::Id::new("live-fit")),
                egui::Id::new("live-fit-id"),
                egui::UiBuilder::new()
                    .max_rect(live)
                    .layout(egui::Layout::top_down(egui::Align::LEFT)),
            );
            draw(app, &mut ui);
            used_h = ui.min_size().y;
        });
        used_h
    }

    /// Every screen must stay compact and top-aligned at the live (tall) window
    /// size — none may spread across the full 926px height.
    #[test]
    fn all_screens_are_compact_at_live_size() {
        let mut app = EtcherApp::build();

        let h = live_used_h(&mut app, |a, ui| a.ui_pick_iso(ui));
        assert!(h.is_finite() && h < 300.0, "PickIso not compact: {h}");

        // PickIso with a selected ISO (shows the file chip).
        app.iso = Some(test_iso());
        let h = live_used_h(&mut app, |a, ui| a.ui_pick_iso(ui));
        assert!(h.is_finite() && h < 400.0, "PickIso+chip not compact: {h}");

        app.step = Step::Confirm;
        app.selected = Some(0);
        let h = live_used_h(&mut app, |a, ui| a.ui_confirm(ui));
        assert!(h.is_finite() && h < 700.0, "Confirm not compact: {h}");

        app.step = Step::Flashing;
        app.hash_done = 1_000_000;
        app.iso_hash_value = Some("abc".into());
        let h = live_used_h(&mut app, |a, ui| a.ui_progress(ui, "Writing"));
        assert!(h.is_finite() && h < 500.0, "Progress not compact: {h}");

        app.done = Some((true, "Success".into()));
        app.done_at = Some(Instant::now());
        app.step = Step::Done;
        let h = live_used_h(&mut app, |a, ui| a.ui_done(ui));
        assert!(h.is_finite() && h < 500.0, "Done not compact: {h}");
    }

    /// Consistency: every screen must fit the same fixed window without
    /// overflowing. If a screen grows taller/wider than the window, this
    /// fails — catching the "bolted on / bunched at the top" regressions.
    #[test]
    fn all_screens_fit_the_same_window() {
        let mut app = EtcherApp::build();

        assert!(screen_fits(&mut app, |a, ui| a.ui_pick_iso(ui)), "PickIso overflows");

        app.step = Step::PickDrive;
        assert!(
            screen_fits(&mut app, |a, ui| a.ui_pick_drive(ui)),
            "PickDrive overflows"
        );

        app.iso = Some(test_iso());
        app.selected = Some(0);
        app.step = Step::Confirm;
        assert!(screen_fits(&mut app, |a, ui| a.ui_confirm(ui)), "Confirm overflows");

        app.step = Step::Flashing;
        assert!(
            screen_fits(&mut app, |a, ui| a.ui_progress(ui, "Writing")),
            "Progress overflows"
        );

        app.done = Some((true, "Success".into()));
        app.done_at = Some(Instant::now());
        app.step = Step::Done;
        assert!(screen_fits(&mut app, |a, ui| a.ui_done(ui)), "Done overflows");
    }
}
