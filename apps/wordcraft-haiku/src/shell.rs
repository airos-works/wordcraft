//! The interface thread's event loop: Haiku's input goes to the window it happened in
//! (`viewports`, which gives every egui viewport a window of its own), WordApp runs its frame in
//! the root viewport's pass, and egui's output goes back to the windows and the system.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use egui::{ViewportCommand, ViewportId};
use wordcraft_ui_egui::WordApp;
use wordcraft_ui_egui::window_geometry::{RawWindowState, WindowGeometry, take_rescue};

use crate::control_server::KeyFile;
use crate::host::{self, RawEvent, kind};
use crate::platform::Haiku;
use crate::save_prefs;
use crate::services::Dialogs;
use crate::viewports::{Viewports, WindowInput};

pub struct Shell {
    pub app: WordApp,
    ctx: egui::Context,
    viewports: Viewports<Haiku>,
    dialogs: Rc<Dialogs>,
    /// The interface scale (the system font against 12 pt) and device pixels per logical pixel
    /// ([`host::device_scale`]), and when the latter was last asked.
    ui_scale: f32,
    dev: f32,
    dev_checked: Instant,
    quit: bool,
    /// The system asked to quit (Command+Q, the Deskbar): the next frame asks about unsaved work.
    quit_asked: bool,
    /// Tracker opened documents: the next frame brings the active window forward.
    activate: bool,
    /// The restored geometry, until the first frame that knows the screen has checked it.
    restored: Option<WindowGeometry>,
    /// This frame's root window state, from its raw input.
    window: RawWindowState,
    /// Files saved (or about to be) that need their MIME type, until when to keep trying.
    to_type: Vec<(PathBuf, Instant)>,
    /// The document's path and dirty flag after the last frame, to notice a save.
    last_saved: (Option<PathBuf>, bool),
    pub key_file: Option<KeyFile>,
}

impl Shell {
    /// The shell around `app`, whose first window (the root viewport's) is native window `root`.
    pub fn new(
        app: WordApp,
        ctx: egui::Context,
        dialogs: Rc<Dialogs>,
        root: i32,
        restored: Option<WindowGeometry>,
        ui_scale: f32,
    ) -> Result<Self, String> {
        let dev = host::device_scale();
        let viewports = Viewports::new(&ctx, Haiku::new()?, root, ui_scale * dev, dev);
        Ok(Self {
            app,
            ctx,
            viewports,
            dialogs,
            ui_scale,
            dev,
            dev_checked: Instant::now(),
            quit: false,
            quit_asked: false,
            activate: false,
            restored: Some(restored.unwrap_or_default()),
            window: RawWindowState::default(),
            to_type: Vec::new(),
            last_saved: (None, true),
            key_file: None,
        })
    }

    /// Runs until the app quits.
    pub fn run(&mut self) {
        while !self.quit {
            host::wait(self.viewports.timeout());
            self.drain();
            if self.dev_checked.elapsed() > Duration::from_secs(2) {
                self.check_density();
            }
            for viewport in self.viewports.take_due() {
                if viewport == ViewportId::ROOT {
                    self.frame();
                } else if self.viewports.run_deferred(viewport).close_root {
                    self.quit = true;
                }
            }
            self.viewports.chores();
        }
    }

    /// Asks for the desktop's density again (the monitors may have changed).
    fn check_density(&mut self) {
        self.dev_checked = Instant::now();
        let dev = host::device_scale();
        if (dev - self.dev).abs() > f32::EPSILON {
            log::info!("display density {} -> {dev}", self.dev);
            self.dev = dev;
            self.viewports.set_density(self.ui_scale * dev, dev);
        }
    }

    /// Hands the queued native events to their windows; the application's own are handled here.
    fn drain(&mut self) {
        while let Some((ev, text)) = host::next_event() {
            if let Some(input) = window_input(&ev, &text) {
                if !self.viewports.input(ev.window, input) {
                    log::debug!("event {} for window {}, which is gone", ev.kind, ev.window);
                }
                continue;
            }
            match ev.kind {
                kind::QUIT_REQUESTED => {
                    self.quit_asked = true;
                    self.viewports.request(ViewportId::ROOT, Duration::ZERO);
                }
                // Tracker's Open, Open With and drops on the icon, and the documents of a
                // later launch (one copy runs: B_SINGLE_LAUNCH). They open as macOS's open
                // events do in the desktop app (`apple_events::poll`): like File › Open,
                // beside the open documents, each in a window of its own. A launch by a
                // document starts on the start screen, as the documents arrive just after.
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
                    self.activate = true;
                    self.viewports.request(ViewportId::ROOT, Duration::ZERO);
                }
                kind::DIALOG_ANSWER => {
                    self.dialogs.answer(ev.id, ev.x != 0.0, text);
                    self.viewports.request(ViewportId::ROOT, Duration::ZERO);
                }
                kind::COLORS => {
                    self.check_density();
                    self.viewports.request(ViewportId::ROOT, Duration::ZERO);
                }
                _ => {}
            }
        }
    }

    /// The desktop app's per-frame window logic, as in `apps/wordcraft`: restored geometry,
    /// tracking the active window's for the preferences, and the close button of the window in
    /// the root viewport (the others' the app handles while drawing them).
    fn logic(&mut self, ctx: &egui::Context) {
        // Quit (Command+Q, the Deskbar's Quit) as the desktop app takes macOS's quit event
        // (`apple_events::poll`): every document with unsaved changes is asked about, each in
        // its window, then the app quits (`quit_requested`).
        if std::mem::take(&mut self.quit_asked) {
            self.app.quit();
        }
        self.app.logic(ctx);
        let window = self.window;
        if let Some((pos, size)) = self.restored.and_then(|g| g.fit_on_monitor(&window)) {
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos));
        }
        // The geometry saved for the next start is the active window's, as the desktop app's:
        // another window's state is the one it last drew with.
        let prev = self.app.ui.window;
        let active = self.app.active_viewport();
        let tracked = if active == ViewportId::ROOT {
            window
        } else {
            ctx.input_for(active, |i| RawWindowState::from_viewport(Some(i.viewport()), Some(i.viewport_rect())))
        };
        self.app.ui.window = WindowGeometry::track(prev, &tracked);
        if let Some(pos) = take_rescue(&mut self.restored, &window) {
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos));
        }
        if std::mem::take(&mut self.activate) {
            ctx.send_viewport_cmd_to(active, ViewportCommand::Focus);
        }
        // The root window's close button: with other windows open only that window closes (the
        // app does it while drawing it); the last one quits, asking about unsaved changes.
        let closing = ctx.input(|i| i.viewport().close_requested());
        let allowed = closing && !self.app.closes_a_window() && self.app.close_requested();
        if (allowed || self.app.quit_requested) && !self.app.printing.busy() {
            self.quit = true;
        }
    }

    /// The root viewport's pass: the app's frame, which draws every other window in its own
    /// viewport on the way.
    fn frame(&mut self) {
        let Some(mut raw) = self.viewports.root_input() else { return };
        self.window = RawWindowState::from_raw(&raw);
        self.app.raw_input_hook(&self.ctx, &mut raw);
        let ctx = self.ctx.clone();
        let t0 = Instant::now();
        let out = ctx.run_ui(raw, |ui| {
            let ctx = ui.ctx().clone();
            self.logic(&ctx);
            self.app.ui(ui);
        });
        let t1 = Instant::now();
        self.type_saved_files();
        if self.viewports.finish(ViewportId::ROOT, out).close_root {
            self.quit = true;
        }
        log::debug!("frame: interface {:.1} ms, output and drawing {:.1} ms", (t1 - t0).as_secs_f64() * 1e3, t1.elapsed().as_secs_f64() * 1e3);
        if !self.to_type.is_empty() {
            // Look again shortly for the files still being written.
            self.viewports.request(ViewportId::ROOT, Duration::from_millis(250));
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

/// A native event that happened in a window, as that window's input.
fn window_input<'a>(ev: &RawEvent, text: &'a str) -> Option<WindowInput<'a>> {
    Some(match ev.kind {
        kind::MOUSE_MOVED | kind::MOUSE_DOWN | kind::MOUSE_UP => {
            WindowInput::Pointer { x: ev.x, y: ev.y, buttons: ev.buttons, modifiers: ev.modifiers }
        }
        kind::POINTER_GONE => WindowInput::PointerGone,
        // A wheel's whole steps, or (`repeat` 1) a touchpad's scroll gesture with its unrounded
        // steps and its phase in `clicks` (`host.cpp`).
        kind::WHEEL => WindowInput::Wheel { dx: ev.dx, dy: ev.dy, modifiers: ev.modifiers, gesture: (ev.repeat != 0).then_some(ev.clicks) },
        kind::KEY_DOWN | kind::KEY_UP => {
            WindowInput::Key { pressed: ev.kind == kind::KEY_DOWN, code: ev.key, raw_char: ev.raw_char, modifiers: ev.modifiers, text }
        }
        kind::MODIFIERS => WindowInput::Modifiers(host::modifiers()),
        kind::FOCUS => WindowInput::Focus(ev.x != 0.0),
        kind::CLOSE => WindowInput::Close,
        kind::DROPPED_FILES => WindowInput::Dropped { x: ev.x, y: ev.y, paths: text.lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect() },
        // The next frame picks up the new size, position or state.
        kind::RESIZED | kind::MOVED | kind::MINIMIZED => WindowInput::Changed,
        // An input method's composition: `id` says what, the pre-edit's selected clause is
        // bytes `key` to `raw_char` (-1: none).
        kind::IME => match ev.id {
            host::ime::PREEDIT => WindowInput::Preedit { text, selection: (ev.key >= 0 && ev.raw_char >= 0).then_some((ev.key, ev.raw_char)) },
            host::ime::COMMIT => WindowInput::Commit(text),
            host::ime::STOPPED => WindowInput::ImeStopped,
            _ => return None,
        },
        _ => return None,
    })
}
