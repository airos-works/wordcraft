//! The window's event loop on the interface thread: Haiku's input becomes egui's, WordApp runs
//! its frame, and egui's output goes back to the window and the system (title, cursor,
//! clipboard, links) and, through Vulkan (`gpu`) or the software rasteriser, to the screen.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Event, Key, Modifiers, PointerButton, Pos2, Theme, ViewportCommand, ViewportEvent, ViewportId, ViewportInfo, pos2, vec2};
use wordcraft_ui_egui::WordApp;
use wordcraft_ui_egui::window_geometry::{RawWindowState, WindowGeometry, take_rescue};

use crate::control_server::KeyFile;
use crate::host::{self, WindowCommand, kind, wheel_phase};
use crate::raster::Rasterizer;
use crate::services::{self, Dialogs};
use crate::{keys, save_prefs};

/// A file dropped on the window.
#[derive(Debug)]
struct DroppedPath(PathBuf);

impl egui::DroppedFile for DroppedPath {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

pub struct Shell {
    pub app: WordApp,
    ctx: egui::Context,
    raster: Rasterizer,
    /// The GPU renderer; `None` draws with `raster` (no hardware Vulkan adapter,
    /// `WORDCRAFT_RENDERER=cpu`, or the device was lost). The rasteriser keeps egui's textures
    /// either way, so it can take over between two frames.
    gpu: Option<crate::gpu::Gpu>,
    dialogs: Rc<Dialogs>,
    start: Instant,
    /// egui's points to device pixels, as of the last frame, and the system's own: the interface
    /// scale (the system font against 12 pt) times the desktop's density.
    ppp: f32,
    native_ppp: f32,
    ui_scale: f32,
    /// Device pixels per logical pixel ([`host::device_scale`]), and when it was last asked.
    dev: f32,
    dev_checked: Instant,
    buttons: i32,
    modifiers: Modifiers,
    focused: bool,
    events: Vec<Event>,
    dropped: Vec<egui::DroppedFileHandle>,
    /// An input method is composing text (a pre-edit is shown).
    composing: bool,
    /// The caret last reported to the host for input methods (logical pixels).
    ime_caret: Option<[f32; 4]>,
    /// A touchpad gesture's end held back for the next frame (its modifiers), see [`Self::wheel`].
    wheel_end: Option<Modifiers>,
    close_requested: bool,
    /// The application was asked to quit (`B_QUIT_REQUESTED`).
    quit_asked: bool,
    quit: bool,
    /// When egui wants the next frame (from this thread's frames).
    repaint_at: Option<Instant>,
    /// When other threads asked for one (`request_repaint` from a worker).
    woken_for: Arc<Mutex<Option<Instant>>>,
    /// The restored geometry, until the first frame that knows the screen has checked it.
    restored: Option<WindowGeometry>,
    /// This frame's window state, from its raw input.
    window: RawWindowState,
    screenshots: Vec<egui::ScreenshotCallback>,
    title: String,
    /// Files saved (or about to be) that need their MIME type, until when to keep trying.
    to_type: Vec<(PathBuf, Instant)>,
    /// The document's path and dirty flag after the last frame, to notice a save.
    last_saved: (Option<PathBuf>, bool),
    pub key_file: Option<KeyFile>,
}

/// egui's cursor as one of Haiku's `BCursorID`s.
fn cursor_id(icon: egui::CursorIcon) -> i32 {
    use egui::CursorIcon as C;
    match icon {
        C::None => 12,
        C::ContextMenu => 3,
        C::Help => 9,
        C::PointingHand => 6,
        C::Progress | C::Wait => 14,
        C::Cell | C::Crosshair => 5,
        C::Text => 2,
        C::VerticalText => 10,
        C::Alias => 29,
        C::Copy => 4,
        C::Move | C::AllScroll => 11,
        C::NoDrop | C::NotAllowed => 13,
        C::Grab => 7,
        C::Grabbing => 8,
        C::ResizeHorizontal | C::ResizeColumn => 24,
        C::ResizeVertical | C::ResizeRow => 23,
        C::ResizeNeSw => 25,
        C::ResizeNwSe => 26,
        C::ResizeEast => 16,
        C::ResizeSouthEast => 21,
        C::ResizeSouth => 17,
        C::ResizeSouthWest => 22,
        C::ResizeWest => 18,
        C::ResizeNorthWest => 20,
        C::ResizeNorth => 15,
        C::ResizeNorthEast => 19,
        C::ZoomIn => 27,
        C::ZoomOut => 28,
        _ => 1,
    }
}

fn button(bit: i32) -> Option<PointerButton> {
    Some(match bit {
        1 => PointerButton::Primary,
        2 => PointerButton::Secondary,
        4 => PointerButton::Middle,
        8 => PointerButton::Extra1,
        16 => PointerButton::Extra2,
        _ => return None,
    })
}

/// Runs `f`; a panic inside it (a wgpu validation failure, a driver gone wrong) becomes an
/// error, so the window falls back to the rasteriser instead of losing the open documents.
fn guard<T>(what: &str, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| Err(format!("{what} failed with an internal error (logged)")))
}

/// The GPU renderer `WORDCRAFT_RENDERER` asks for: `auto` (the default: Vulkan on a hardware
/// adapter, else the rasteriser), `vulkan` (Vulkan or an error) or `cpu`.
fn start_gpu() -> Result<Option<crate::gpu::Gpu>, String> {
    let mode = std::env::var("WORDCRAFT_RENDERER").unwrap_or_else(|_| "auto".into());
    let gpu = match mode.as_str() {
        "cpu" => None,
        "auto" | "vulkan" => match guard("GPU startup", crate::gpu::Gpu::new) {
            Ok(gpu) => Some(gpu),
            Err(e) if mode == "vulkan" => return Err(e),
            Err(e) => {
                log::warn!("GPU startup: {e}; using the software rasteriser");
                None
            }
        },
        _ => return Err("WORDCRAFT_RENDERER must be auto, cpu or vulkan".into()),
    };
    log::info!("air/OS renderer: {}", if gpu.is_some() { "Vulkan" } else { "CPU" });
    Ok(gpu)
}

impl Shell {
    pub fn new(app: WordApp, ctx: egui::Context, dialogs: Rc<Dialogs>, restored: Option<WindowGeometry>, ui_scale: f32) -> Result<Self, String> {
        let gpu = start_gpu()?;
        let dev = host::device_scale();
        let native_ppp = ui_scale * dev;
        let woken_for: Arc<Mutex<Option<Instant>>> = Arc::default();
        let wake = woken_for.clone();
        ctx.set_request_repaint_callback(move |info| {
            let at = Instant::now() + info.delay;
            if let Ok(mut w) = wake.lock() {
                *w = Some(w.map_or(at, |old| old.min(at)));
            }
            host::wake();
        });
        Ok(Self {
            app,
            ctx,
            raster: Rasterizer::new(),
            gpu,
            dialogs,
            start: Instant::now(),
            ppp: native_ppp,
            native_ppp,
            ui_scale,
            dev,
            dev_checked: Instant::now(),
            buttons: 0,
            modifiers: Modifiers::default(),
            focused: true,
            events: Vec::new(),
            dropped: Vec::new(),
            wheel_end: None,
            composing: false,
            ime_caret: None,
            close_requested: false,
            quit_asked: false,
            quit: false,
            repaint_at: Some(Instant::now()),
            woken_for,
            restored: Some(restored.unwrap_or_default()),
            window: RawWindowState::default(),
            screenshots: Vec::new(),
            title: String::new(),
            to_type: Vec::new(),
            last_saved: (None, true),
            key_file: None,
        })
    }

    /// Runs until the app quits.
    pub fn run(&mut self) {
        while !self.quit {
            host::wait(self.timeout());
            let happened = self.drain();
            let due = self.take_due();
            if happened || due {
                self.frame();
            }
            self.present_chores();
        }
    }

    /// What the presenter does between frames: the last frame again where the window moved on
    /// the screen, and app_server's copy catching up with frames that went straight to the screen.
    fn present_chores(&mut self) {
        let dev = self.dev;
        let Some(gpu) = &mut self.gpu else { return };
        let result = if gpu.wants_present() {
            guard("GPU present", || gpu.present_again(dev))
        } else if gpu.sync_due().is_some_and(|at| at <= Instant::now()) {
            guard("GPU sync", || gpu.sync(dev))
        } else {
            return;
        };
        if let Err(e) = result {
            self.drop_gpu(&e);
        }
    }

    /// Back to the software rasteriser for good: the device failed (`why`). The next frame
    /// draws everything with it.
    fn drop_gpu(&mut self, why: &str) {
        if self.gpu.take().is_some() {
            log::error!("GPU: {why}; switching to the software rasteriser");
            self.raster.invalidate();
            self.repaint_at = Some(Instant::now());
        }
    }

    /// When the next frame is due, if one is.
    fn next_due(&self) -> Option<Instant> {
        let woken = self.woken_for.lock().ok().and_then(|w| *w);
        let sync = self.gpu.as_ref().and_then(|g| g.sync_due());
        [self.repaint_at, woken, sync].into_iter().flatten().min()
    }

    fn timeout(&self) -> Option<Duration> {
        self.next_due().map(|at| at.saturating_duration_since(Instant::now()))
    }

    /// Whether a frame is due now; clears what made it due.
    fn take_due(&mut self) -> bool {
        let now = Instant::now();
        let due = self.next_due().is_some_and(|at| at <= now);
        if due {
            self.repaint_at = None;
            if let Ok(mut w) = self.woken_for.lock()
                && w.is_some_and(|at| at <= now)
            {
                *w = None;
            }
        }
        due
    }

    /// A point in the window (logical pixels) in egui's points.
    fn pos(&self, x: f32, y: f32) -> Pos2 {
        pos2(x * self.dev / self.ppp, y * self.dev / self.ppp)
    }

    /// Asks for the desktop's density again (the monitors may have changed).
    fn check_density(&mut self) {
        self.dev_checked = Instant::now();
        let dev = host::device_scale();
        if (dev - self.dev).abs() > f32::EPSILON {
            log::info!("display density {} -> {dev}", self.dev);
            self.dev = dev;
            self.native_ppp = self.ui_scale * dev;
        }
    }

    fn set_modifiers(&mut self, bits: u32) {
        let m = keys::modifiers(bits);
        if m != self.modifiers {
            self.modifiers = m;
            self.events.push(Event::ModifiersChanged(m));
        }
    }

    /// Turns the queued native events into egui's. Whether there were any.
    fn drain(&mut self) -> bool {
        let mut any = false;
        if let Some(modifiers) = self.wheel_end.take() {
            any = true;
            self.events.push(Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::Vec2::ZERO,
                phase: egui::TouchPhase::End,
                source: egui::MouseWheelSource::Trackpad,
                modifiers,
            });
        }
        while let Some((ev, text)) = host::next_event() {
            any = true;
            match ev.kind {
                kind::MOUSE_MOVED | kind::MOUSE_DOWN | kind::MOUSE_UP => {
                    self.set_modifiers(ev.modifiers);
                    let pos = self.pos(ev.x, ev.y);
                    self.events.push(Event::PointerMoved(pos));
                    let changed = ev.buttons ^ self.buttons;
                    for bit in [1, 2, 4, 8, 16] {
                        if changed & bit != 0
                            && let Some(b) = button(bit)
                        {
                            self.events.push(Event::PointerButton { pos, button: b, pressed: ev.buttons & bit != 0, modifiers: self.modifiers });
                        }
                    }
                    self.buttons = ev.buttons;
                }
                kind::POINTER_GONE => self.events.push(Event::PointerGone),
                kind::WHEEL => {
                    self.set_modifiers(ev.modifiers);
                    self.wheel(&ev);
                }
                kind::KEY_DOWN | kind::KEY_UP => self.key(ev.kind == kind::KEY_DOWN, ev.key, ev.raw_char, ev.modifiers, &text),
                kind::MODIFIERS => self.set_modifiers(host::modifiers()),
                kind::FOCUS => {
                    self.focused = ev.x != 0.0;
                    self.events.push(Event::WindowFocused(self.focused));
                    if !self.focused {
                        // Keys released elsewhere never come back up.
                        self.set_modifiers(0);
                    }
                }
                kind::CLOSE => self.close_requested = true,
                kind::QUIT_REQUESTED => self.quit_asked = true,
                // Tracker's Open, Open With and drops on the icon, and the documents of a
                // later launch (one copy runs: B_SINGLE_LAUNCH). They open as macOS's open
                // events do in the desktop app (`apple_events::poll`): like File › Open,
                // beside the open documents. A launch by a document starts on the start
                // screen, as the documents arrive just after.
                kind::OPEN_FILES => {
                    for path in text.lines().filter(|l| !l.is_empty()) {
                        match self.app.open_file(path, None) {
                            // Leave Backstage, as opening from the Open dialog does.
                            Ok(_) => self.app.ui.backstage = false,
                            Err(e) => {
                                log::warn!("{path}: {e}");
                                let msg = wordcraft_ui_egui::tl!("Couldn't open {path}: {error}");
                                self.app.status(wordcraft_ui_egui::i18n::fmt(msg, &[("path", path), ("error", &e)]));
                            }
                        }
                    }
                    host::window_command(WindowCommand::Activate);
                }
                kind::DROPPED_FILES => {
                    self.events.push(Event::PointerMoved(self.pos(ev.x, ev.y)));
                    for path in text.lines().filter(|l| !l.is_empty()) {
                        self.dropped.push(Arc::new(DroppedPath(PathBuf::from(path))));
                    }
                }
                kind::DIALOG_ANSWER => self.dialogs.answer(ev.id, ev.x != 0.0, text),
                kind::IME => self.input_method(&ev, text),
                kind::COLORS => self.check_density(),
                // The next frame picks up the new size, position or state.
                kind::RESIZED | kind::MOVED | kind::MINIMIZED => {}
                _ => {}
            }
        }
        any
    }

    /// A wheel turn, or a step of an air/OS touchpad's scroll gesture: its precise deltas (wheel
    /// steps, unrounded) become points at egui's line scroll speed, with the gesture's phases,
    /// so the canvas follows the fingers 1:1 and coasts after they lift, and egui's scroll areas
    /// treat it as a trackpad. A wheel's whole steps stay lines.
    fn wheel(&mut self, ev: &host::RawEvent) {
        use egui::{MouseWheelSource, MouseWheelUnit, TouchPhase};
        let modifiers = self.modifiers;
        // `repeat`: precise deltas (`be:wheel_precise_delta_*`), `clicks`: `be:wheel_phase`.
        if ev.repeat == 0 {
            // A mouse wheel (B_MOUSE_POINTING_DEVICE), or a system without touchpad gestures.
            self.events.push(Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: vec2(-ev.dx, -ev.dy),
                phase: TouchPhase::Move,
                source: MouseWheelSource::Wheel,
                modifiers,
            });
            return;
        }
        let event = |unit, delta, phase| Event::MouseWheel { unit, delta, phase, source: MouseWheelSource::Trackpad, modifiers };
        let line = self.ctx.options(|o| o.input_options.line_scroll_speed);
        let delta = vec2(-ev.dx, -ev.dy) * line;
        let delta = if delta.is_finite() { delta } else { egui::Vec2::ZERO };
        match ev.clicks {
            wheel_phase::BEGAN => {
                self.events.push(event(MouseWheelUnit::Point, egui::Vec2::ZERO, TouchPhase::Start));
            }
            wheel_phase::ENDED => {
                // egui drops a gesture's unapplied deltas at its end: one that ends in the same
                // frame as its last movement ends in the next.
                let moved = self.events.iter().any(|e| matches!(e, Event::MouseWheel { source: MouseWheelSource::Trackpad, phase: TouchPhase::Move, .. }));
                if moved {
                    self.wheel_end = Some(modifiers);
                } else {
                    self.events.push(event(MouseWheelUnit::Point, egui::Vec2::ZERO, TouchPhase::End));
                }
                return;
            }
            _ => {}
        }
        if delta != egui::Vec2::ZERO {
            self.events.push(event(MouseWheelUnit::Point, delta, TouchPhase::Move));
        }
    }

    /// An input method's composition (Japanese, Chinese, Korean…) as egui's IME events: the text
    /// being composed is shown as a pre-edit at the caret, the confirmed text is typed.
    fn input_method(&mut self, ev: &host::RawEvent, text: String) {
        use egui::ImeEvent;
        match ev.id {
            host::ime::PREEDIT => {
                // The clause being converted, from bytes to characters.
                let chars = |b: i32| usize::try_from(b).ok().and_then(|b| text.get(..b)).map(|s| s.chars().count());
                let active_range_chars = match (chars(ev.key), chars(ev.raw_char)) {
                    (Some(start), Some(end)) if start <= end => Some(start..end),
                    _ => None,
                };
                self.composing = !text.is_empty();
                self.events.push(Event::Ime(ImeEvent::Preedit { text, active_range_chars }));
            }
            host::ime::COMMIT => {
                self.composing = false;
                self.events.push(Event::Ime(ImeEvent::Commit(text)));
            }
            host::ime::STOPPED => {
                // Stopped without confirming: the pre-edit goes.
                if std::mem::take(&mut self.composing) {
                    self.events.push(Event::Ime(ImeEvent::Preedit { text: String::new(), active_range_chars: None }));
                }
            }
            _ => {}
        }
    }

    fn key(&mut self, pressed: bool, code: i32, raw_char: i32, bits: u32, text: &str) {
        self.set_modifiers(bits);
        let (logical, physical) = keys::key(code, raw_char, text.as_bytes());
        log::debug!(
            "key {} code {code:#x} raw {raw_char:#x} modifiers {bits:#x} {text:?}: {logical:?} / {physical:?}",
            if pressed { "down" } else { "up" }
        );
        let m = self.modifiers;
        if let Some(active) = logical.or(physical) {
            if pressed {
                // As egui's own integrations do: the clipboard shortcuts become clipboard events.
                if m.command && active == Key::X || m.shift && active == Key::Delete {
                    self.events.push(Event::Cut);
                    return;
                }
                if m.command && (active == Key::C || active == Key::Insert) {
                    self.events.push(Event::Copy);
                    return;
                }
                if m.command && active == Key::V || m.shift && active == Key::Insert {
                    if let Some(t) = services::clipboard_text().map(|t| t.replace("\r\n", "\n")) {
                        self.events.push(Event::Paste(t));
                    }
                    return;
                }
            }
            self.events.push(Event::Key { key: active, physical_key: physical, pressed, repeat: false, modifiers: m });
        }
        if pressed && let Some(t) = keys::text(bits, text) {
            self.events.push(Event::Text(t.to_string()));
        }
    }

    /// Tells the input method where the caret is, and ends its composition when the editor
    /// interrupts it or nothing takes typing any more.
    fn input_method_output(&mut self, ime: Option<&egui::output::IMEOutput>) {
        let k = self.ppp / self.dev;
        let caret = ime.map(|o| o.cursor_rect).filter(|r| r.is_finite()).map(|r| [r.min.x * k, r.min.y * k, r.max.x * k, r.max.y * k]);
        if caret != self.ime_caret {
            self.ime_caret = caret;
            host::ime_caret(caret);
        }
        if ime.is_none_or(|o| o.should_interrupt_composition) {
            host::ime_stop();
            if std::mem::take(&mut self.composing) {
                self.events.push(Event::Ime(egui::ImeEvent::Preedit { text: String::new(), active_range_chars: None }));
            }
        }
    }

    fn viewport_info(&self, close: bool) -> ViewportInfo {
        let w = host::window_info();
        let k = self.dev / self.ppp;
        let rect = |r: [f32; 4]| egui::Rect::from_min_max(pos2(r[0], r[1]) * k, pos2(r[2], r[3]) * k);
        ViewportInfo {
            title: Some(self.title.clone()),
            events: if close { vec![ViewportEvent::Close] } else { Vec::new() },
            native_pixels_per_point: Some(self.native_ppp),
            monitor_size: (w.screen[0] > 0.0).then(|| vec2(w.screen[0], w.screen[1]) * k),
            inner_rect: Some(rect(w.inner)),
            outer_rect: Some(rect(w.outer)),
            minimized: Some(w.minimized),
            maximized: Some(false),
            fullscreen: Some(false),
            focused: Some(w.active),
            ..Default::default()
        }
    }

    /// The desktop app's per-frame window logic, as in `apps/wordcraft`: restored geometry,
    /// tracking it for the preferences, and the window's close button.
    fn logic(&mut self, ctx: &egui::Context) {
        // Quit (Command+Q, the Deskbar's Quit) as the desktop app takes macOS's quit event
        // (`apple_events::poll`): every document with unsaved changes is asked about, then the
        // app quits (`quit_requested`). A close request would close only the shown document's
        // window while the running copy has several documents open.
        if std::mem::take(&mut self.quit_asked) {
            self.app.quit();
        }
        self.app.logic(ctx);
        let window = self.window;
        if let Some((pos, size)) = self.restored.and_then(|g| g.fit_on_monitor(&window)) {
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos));
        }
        let prev = self.app.ui.window;
        self.app.ui.window = WindowGeometry::track(prev, &window);
        if let Some(pos) = take_rescue(&mut self.restored, &window) {
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos));
        }
        // One native window: the window's close button closes any embedded document window the
        // app has open first, then quits, asking about unsaved changes.
        let closing = ctx.input(|i| i.viewport().close_requested());
        let allowed = closing && !self.app.closes_a_window() && self.app.close_requested();
        if (allowed || self.app.quit_requested) && !self.app.printing.busy() {
            self.quit = true;
        }
    }

    fn frame(&mut self) {
        if self.gpu.as_ref().is_some_and(|g| g.faulted()) {
            self.drop_gpu("the device was lost or failed");
        }
        if self.dev_checked.elapsed() > Duration::from_secs(2) {
            self.check_density();
        }
        let (lw, lh) = host::view_size();
        let (w, h) = ((lw * self.dev).round(), (lh * self.dev).round());
        if w < 1.0 || h < 1.0 {
            self.repaint_at = Some(Instant::now() + Duration::from_millis(50));
            return;
        }
        let close = std::mem::take(&mut self.close_requested);
        let raw = egui::RawInput {
            viewport_id: ViewportId::ROOT,
            viewports: std::iter::once((ViewportId::ROOT, self.viewport_info(close))).collect(),
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, vec2(w, h) / self.ppp)),
            max_texture_side: Some(self.gpu.as_ref().map_or(8192, |g| g.max_texture_side().min(16384))),
            time: Some(self.start.elapsed().as_secs_f64()),
            predicted_dt: 1.0 / 60.0,
            events: std::mem::take(&mut self.events),
            dropped_files: std::mem::take(&mut self.dropped),
            focused: self.focused,
            system_theme: Some(if host::dark() { Theme::Dark } else { Theme::Light }),
            ..Default::default()
        };
        let mut raw = raw;
        self.window = RawWindowState::from_raw(&raw);
        self.app.raw_input_hook(&self.ctx, &mut raw);
        let ctx = self.ctx.clone();
        let t0 = Instant::now();
        let out = ctx.run_ui(raw, |ui| {
            let ctx = ui.ctx().clone();
            self.logic(&ctx);
            self.app.ui(ui);
        });

        self.type_saved_files();

        // The system side of egui's output.
        host::set_cursor(cursor_id(out.platform_output.cursor_icon));
        self.input_method_output(out.platform_output.ime.as_ref());
        for cmd in &out.platform_output.commands {
            log::debug!("output {cmd:?}");
            match cmd {
                egui::OutputCommand::CopyText(t) => {
                    if !host::clipboard_set(t, None, None) {
                        log::warn!("clipboard: couldn't copy");
                    }
                }
                egui::OutputCommand::CopyImage(img) => copy_image(img),
                // Haiku has one clipboard, no X11-style selection to keep the selected text in.
                egui::OutputCommand::TextSelectionSettled(_) => {}
                egui::OutputCommand::OpenUrl(u) => {
                    if !host::open_url(&u.url) {
                        log::warn!("couldn't open {}", u.url);
                    }
                }
            }
        }
        let mut delay = Duration::MAX;
        if let Some(vp) = out.viewport_output.get(&ViewportId::ROOT) {
            delay = vp.repaint_delay;
            if let Some(t) = &vp.builder.title
                && *t != self.title
            {
                self.title.clone_from(t);
                host::set_title(t);
            }
            for cmd in &vp.commands {
                self.command(cmd);
            }
        }

        // Draw.
        let ppp = out.pixels_per_point;
        self.raster.set_textures(&out.textures_delta);
        let minimized = host::window_info().minimized;
        if !minimized {
            let t1 = Instant::now();
            let prims = ctx.tessellate(out.shapes, ppp);
            let t2 = Instant::now();
            let clear = ctx.global_style().visuals.panel_fill;
            let (pw, ph) = (w as usize, h as usize);
            let dev = self.dev;
            // The presenter shows GPU frames itself; the rasteriser's are shown below.
            let mut failed = None;
            let dirty = match &mut self.gpu {
                Some(gpu) => {
                    let size = [u32::try_from(pw).unwrap_or(u32::MAX), u32::try_from(ph).unwrap_or(u32::MAX)];
                    if let Err(e) = guard("GPU frame", || gpu.render(size, &prims, &out.textures_delta, ppp, clear, dev)) {
                        failed = Some(e);
                    }
                    None
                }
                None => self.raster.render([pw, ph], &prims, ppp, clear),
            };
            if let Some(e) = failed {
                self.drop_gpu(&e);
            }
            let t3 = Instant::now();
            if let Some(dirty) = dirty {
                host::present(self.raster.pixels(), pw, ph, dirty, self.dev);
            }
            log::debug!(
                "frame {pw}x{ph}: ui {:.1} ms, tessellate {:.1} ms, raster {:.1} ms, present {:.1} ms, dirty {dirty:?}",
                (t1 - t0).as_secs_f64() * 1e3,
                (t2 - t1).as_secs_f64() * 1e3,
                (t3 - t2).as_secs_f64() * 1e3,
                t3.elapsed().as_secs_f64() * 1e3
            );
        }
        self.raster.free_textures(&out.textures_delta);
        for callback in std::mem::take(&mut self.screenshots) {
            let image = match self.gpu.as_mut() {
                Some(gpu) => gpu.image().unwrap_or_else(|e| {
                    log::warn!("screenshot: {e}");
                    egui::ColorImage::default()
                }),
                None => self.raster.image(),
            };
            callback.complete(Arc::new(image));
            delay = Duration::ZERO;
        }
        if (ppp - self.ppp).abs() > f32::EPSILON {
            self.ppp = ppp;
            delay = Duration::ZERO;
        }
        if !self.events.is_empty() || self.wheel_end.is_some() {
            delay = Duration::ZERO;
        }
        if !self.to_type.is_empty() {
            // Look again shortly for the files still being written.
            delay = delay.min(Duration::from_millis(250));
        }
        self.repaint_at = (delay < Duration::from_secs(3600)).then(|| Instant::now() + delay);
    }

    fn command(&mut self, cmd: &ViewportCommand) {
        // Points to logical pixels.
        let ppp = self.ppp / self.dev;
        match cmd {
            ViewportCommand::Close => self.quit = true,
            ViewportCommand::Title(t) => {
                if *t != self.title {
                    self.title.clone_from(t);
                    host::set_title(t);
                }
            }
            ViewportCommand::InnerSize(s) => host::window_command(WindowCommand::ResizeTo(s.x * ppp, s.y * ppp)),
            ViewportCommand::MinInnerSize(s) => host::window_command(WindowCommand::MinSize(s.x * ppp, s.y * ppp)),
            ViewportCommand::OuterPosition(p) => host::window_command(WindowCommand::MoveTo(p.x * ppp, p.y * ppp)),
            ViewportCommand::Minimized(m) => host::window_command(WindowCommand::Minimize(*m)),
            ViewportCommand::Maximized(true) => host::window_command(WindowCommand::Zoom),
            ViewportCommand::Focus => host::window_command(WindowCommand::Activate),
            ViewportCommand::Screenshot(data) => self.screenshots.push(data.clone()),
            ViewportCommand::RequestCut => self.events.push(Event::Cut),
            ViewportCommand::RequestCopy => self.events.push(Event::Copy),
            ViewportCommand::RequestPaste => {
                if let Some(t) = services::clipboard_text() {
                    self.events.push(Event::Paste(t.replace("\r\n", "\n")));
                }
            }
            _ => {}
        }
    }

    /// Files WordCraft saves are written by the engine with plain file calls; Haiku programs'
    /// files carry their MIME type, which Tracker opens them by. A document that turned clean
    /// with a path was just saved; a Save panel's path is written a frame or two later.
    fn type_saved_files(&mut self) {
        let now = Instant::now();
        let state = (self.app.session.path.clone(), self.app.session.dirty);
        if let (Some(path), false) = &state
            && (self.last_saved.1 || self.last_saved.0.as_ref() != Some(path))
        {
            self.to_type.push((path.clone(), now + Duration::from_secs(10)));
        }
        self.last_saved = state;
        for path in self.dialogs.saved.borrow_mut().drain(..) {
            self.to_type.push((path, now + Duration::from_secs(10)));
        }
        self.to_type.retain(|(path, until)| {
            if path.exists() {
                if !host::type_file(path) {
                    log::debug!("couldn't set the type of {}", path.display());
                }
                false
            } else {
                now < *until
            }
        });
    }

    /// What quitting does besides closing the window (`on_exit` in the desktop app).
    pub fn finish(&mut self) {
        self.settle_recovery();
        save_prefs(&self.app);
        if let Some(key_file) = &self.key_file {
            key_file.remove();
        }
    }

    /// A document saved just before quitting (Save in the Save Changes prompt) keeps its
    /// recovery copy until the recovery worker has discarded it, a frame or two later: wait for
    /// that, briefly, or the next start offers to recover a document that was saved.
    fn settle_recovery(&mut self) {
        let session = &mut self.app.session;
        let until = Instant::now() + Duration::from_secs(2);
        while !session.dirty && Instant::now() < until {
            session.recovery_tick();
            if !session.recovery_busy() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// egui's image copy, as PNG.
fn copy_image(img: &egui::ColorImage) {
    let [w, h] = img.size;
    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
    let mut png = Vec::new();
    let encoded = image::RgbaImage::from_raw(w as u32, h as u32, rgba)
        .ok_or_else(|| "bad size".to_string())
        .and_then(|i| i.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|e| e.to_string()));
    match encoded {
        Ok(()) => {
            if !host::clipboard_set_picture("image/png", &png) {
                log::warn!("clipboard: couldn't copy the picture");
            }
        }
        Err(e) => log::warn!("clipboard picture: {e}"),
    }
}
