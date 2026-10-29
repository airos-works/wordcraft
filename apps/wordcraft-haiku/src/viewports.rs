//! Every egui viewport in a native window of its own, as eframe gives them on the other desktops.
//!
//! egui shows extra windows as viewports: *immediate* ones (`show_viewport_immediate`, what
//! upstream's View › New Window, DeckCraft's presenter view and craft-ui's floating panels use),
//! drawn while their parent's pass runs, and *deferred* ones (`show_viewport_deferred`), which
//! repaint on their own. With `embed_viewports` off, egui hands each viewport to the host, which
//! has to open a window for it, give it its own input, run its pass and draw it. That is this
//! module, kept apart from the rest of the host so the other Craft hosts can take it as it is:
//!
//! - [`Platform`] is all it needs from the system: open, close and command native windows, tell
//!   where they are, draw a pass's triangles into one, and the clipboard, cursor and URL bits of
//!   egui's output. `platform.rs` implements it with the Be API (`host.cpp`'s `BDirectWindow`s)
//!   and Vulkan or the software rasteriser; the tests below with a fake.
//! - Input arrives tagged with its native window ([`Viewports::input`]) and becomes that
//!   viewport's egui events, pointer buttons, modifiers and focus. Each pass gets its own
//!   [`egui::RawInput`] with every viewport's [`ViewportInfo`], as eframe's does.
//! - The root viewport's pass is the host's ([`Viewports::root_input`], then [`Viewports::finish`]
//!   with its output): the app's logic and the raw-input hook stay the host's. egui calls the
//!   immediate viewport renderer this module installs from inside that pass, for every immediate
//!   viewport; a deferred viewport's pass is [`Viewports::run_deferred`].
//! - Every pass's output is handled the same way ([`Viewports::finish`]): the platform output
//!   (cursor, clipboard, links), new viewports (deferred ones get a window here, immediate ones in
//!   the renderer), builder changes and [`ViewportCommand`]s become native window commands, the
//!   pass is drawn into its window, and once the outermost pass is done, windows whose viewport
//!   egui dropped close.
//! - Repaints: an immediate viewport is drawn by its parent's pass, so input for it or a repaint
//!   it asks for runs the root's (or its deferred ancestor's) pass. [`Viewports::take_due`] says
//!   which passes are due.
//!
//! What the windows do when closed is the app's, as on the desktop: the close button puts
//! [`ViewportEvent::Close`] in that viewport's input (`close_requested`), and the window stays
//! until the app stops showing the viewport. For the root viewport the host decides (WordCraft:
//! quit when it is the last window). A [`ViewportCommand::Close`] to the root comes back in
//! [`Finished::close_root`].
//!
//! Coordinates: the platform speaks logical pixels (the desktop's coordinates); egui points are
//! `logical × scale / pixels_per_point`, where `scale` is device pixels per logical pixel.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use egui::{
    Context, CursorIcon, DeferredViewportUiCallback, Event, FullOutput, ImmediateViewport, Key, Modifiers, PointerButton, RawInput, ResizeDirection,
    Theme, ViewportBuilder, ViewportClass, ViewportCommand, ViewportEvent, ViewportId, ViewportIdPair, ViewportInfo, WindowLevel, pos2, vec2,
};

use crate::keys;

/// A native window, as the platform names it.
pub type WindowId = i32;

/// Where and how a native window opens, in logical pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowSpec {
    pub title: String,
    /// The frame's top-left corner, border and tab included; `None` lets the system place it.
    pub position: Option<[f32; 2]>,
    /// Content size.
    pub size: [f32; 2],
    pub min_size: Option<[f32; 2]>,
    pub max_size: Option<[f32; 2]>,
    pub decorations: bool,
    pub resizable: bool,
    pub visible: bool,
    pub zoomed: bool,
    pub fullscreen: bool,
    /// Above other windows ([`WindowLevel::AlwaysOnTop`]).
    pub floating: bool,
    /// The title bar's close, minimise and zoom buttons.
    pub buttons: [bool; 3],
}

/// What a native window is asked to do, in logical pixels.
#[derive(Clone, Debug, PartialEq)]
pub enum NativeCommand {
    Title(String),
    /// The frame's top-left corner, border included.
    MoveTo([f32; 2]),
    /// Content size.
    ResizeTo([f32; 2]),
    MinSize([f32; 2]),
    MaxSize(Option<[f32; 2]>),
    Minimize(bool),
    Zoom,
    Activate,
    Show(bool),
    Decorations(bool),
    Resizable(bool),
    Floating(bool),
    Fullscreen(bool),
    /// Move the window with the pointer while its button stays down.
    StartDrag,
    /// Move these edges ([`edges`]) with the pointer while its button stays down.
    BeginResize(u32),
    /// Close, minimise and zoom buttons.
    Buttons([bool; 3]),
}

/// The phases of a touchpad's scroll gesture ([`WindowInput::Wheel`]), air/OS's
/// `be:wheel_phase` (`input_wheel_phase` in `<Input.h>`).
pub mod gesture {
    pub const BEGAN: i32 = 1;
    pub const ENDED: i32 = 3;
}

/// Edges for [`NativeCommand::BeginResize`].
pub mod edges {
    pub const LEFT: u32 = 1;
    pub const RIGHT: u32 = 2;
    pub const TOP: u32 = 4;
    pub const BOTTOM: u32 = 8;
}

/// Where a native window is, in logical pixels: `[left, top, right, bottom)`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowInfo {
    /// Its content.
    pub inner: [f32; 4],
    /// Its frame, border and tab included.
    pub outer: [f32; 4],
    /// The monitor most of it is on.
    pub monitor: [f32; 4],
    pub minimized: bool,
    pub active: bool,
    pub fullscreen: bool,
    pub hidden: bool,
}

/// Input that happened in a native window, in logical pixels within its content.
#[derive(Debug)]
pub enum WindowInput<'a> {
    /// The pointer moved or its buttons changed (bits: 1 primary, 2 secondary, 4 middle, 8 and
    /// 16 the extra buttons).
    Pointer {
        x: f32,
        y: f32,
        buttons: i32,
        modifiers: u32,
    },
    PointerGone,
    /// A wheel's steps, or a step of a touchpad's scroll gesture: its unrounded steps and the
    /// gesture's phase ([`gesture`]).
    Wheel {
        dx: f32,
        dy: f32,
        modifiers: u32,
        gesture: Option<i32>,
    },
    /// A key, with the platform's key code, its unmodified character and the text it typed.
    Key {
        pressed: bool,
        code: i32,
        raw_char: i32,
        modifiers: u32,
        text: &'a str,
    },
    Modifiers(u32),
    Focus(bool),
    /// The close button (or the system) asked the window to close.
    Close,
    Dropped {
        x: f32,
        y: f32,
        paths: Vec<PathBuf>,
    },
    /// Moved, resized, minimised or restored: the next pass shows it.
    Changed,
    /// An input method's text being composed, and the clause being converted in it (bytes).
    Preedit {
        text: &'a str,
        selection: Option<(i32, i32)>,
    },
    /// An input method's confirmed text.
    Commit(&'a str),
    /// The input method stopped (without confirming, if a pre-edit is shown).
    ImeStopped,
}

/// What the module needs from the system. Window calls name windows [`Platform::open`] made.
pub trait Platform: 'static {
    /// Ends the interface thread's wait for input; called from any thread.
    fn waker(&self) -> fn();
    fn open(&mut self, spec: &WindowSpec) -> Option<WindowId>;
    /// Closes the window, and lets go of what was drawn for it.
    fn close(&mut self, window: WindowId);
    fn command(&mut self, window: WindowId, command: NativeCommand);
    fn info(&self, window: WindowId) -> WindowInfo;
    fn set_cursor(&mut self, window: WindowId, cursor: CursorIcon);
    /// Clipboard and links.
    fn output(&mut self, command: &egui::OutputCommand);
    fn clipboard_text(&self) -> Option<String>;
    fn dark(&self) -> bool;
    fn max_texture_side(&self) -> usize;
    /// egui's texture uploads (`delta.set`), for every window's frames.
    fn set_textures(&mut self, delta: &egui::TexturesDelta);
    fn free_textures(&mut self, ids: &[egui::TextureId]);
    /// Draws a pass into its window: `size` device pixels, `scale` per logical pixel.
    fn paint(
        &mut self,
        window: WindowId,
        size: [u32; 2],
        primitives: &[egui::ClippedPrimitive],
        pixels_per_point: f32,
        clear: egui::Color32,
        scale: f32,
    );
    /// The window's last frame.
    fn screenshot(&mut self, window: WindowId) -> egui::ColorImage;
    /// Where the caret is in the window (logical pixels: left, top, right, bottom) for an input
    /// method's candidate window, or `None` when nothing there takes typing.
    fn ime_caret(&mut self, _window: WindowId, _caret: Option<[f32; 4]>) {}
    /// Ends the input method's composition in the window, if one is going on.
    fn ime_stop(&mut self, _window: WindowId) {}
    /// When the presenter has work between frames, and doing it.
    fn chores_due(&self) -> Option<Instant> {
        None
    }
    fn chores(&mut self, _scale: f32) {}
}

/// A file dropped on a window.
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

/// What the host has to do after a pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Finished {
    /// The app sent [`ViewportCommand::Close`] to the root viewport.
    pub close_root: bool,
}

/// One viewport and its window.
struct Window {
    ids: ViewportIdPair,
    class: ViewportClass,
    /// `None` when the platform couldn't open one.
    native: Option<WindowId>,
    /// What the window was made or last changed from.
    builder: ViewportBuilder,
    info: ViewportInfo,
    events: Vec<Event>,
    dropped: Vec<egui::DroppedFileHandle>,
    buttons: i32,
    modifiers: Modifiers,
    focused: bool,
    /// egui's points to device pixels, as of its last pass.
    pixels_per_point: f32,
    /// The size (device pixels) of the pass in progress, drawn when it finishes.
    size: [u32; 2],
    deferred: Option<Arc<DeferredViewportUiCallback>>,
    screenshots: Vec<egui::ScreenshotCallback>,
    /// A touchpad gesture's end held back for the pass after the next (its modifiers), and the
    /// one for the next pass ([`wheel`]).
    wheel_end: Option<Modifiers>,
    wheel_end_next: Option<Modifiers>,
    /// An input method is composing text in the window (a pre-edit is shown).
    composing: bool,
    /// The caret last told to the platform for input methods (logical pixels).
    ime_caret: Option<[f32; 4]>,
}

impl Window {
    fn new(ids: ViewportIdPair, class: ViewportClass, native: Option<WindowId>, builder: ViewportBuilder, pixels_per_point: f32) -> Self {
        Self {
            ids,
            class,
            native,
            builder,
            info: ViewportInfo { parent: (ids.this != ViewportId::ROOT).then_some(ids.parent), ..ViewportInfo::default() },
            events: Vec::new(),
            dropped: Vec::new(),
            buttons: 0,
            modifiers: Modifiers::default(),
            focused: true,
            pixels_per_point,
            size: [0, 0],
            deferred: None,
            screenshots: Vec::new(),
            wheel_end: None,
            wheel_end_next: None,
            composing: false,
            ime_caret: None,
        }
    }

    fn set_modifiers(&mut self, bits: u32) {
        let m = keys::modifiers(bits);
        if m != self.modifiers {
            self.modifiers = m;
            self.events.push(Event::ModifiersChanged(m));
        }
    }

    /// A point in the window (logical pixels) in egui's points.
    fn pos(&self, x: f32, y: f32, scale: f32) -> egui::Pos2 {
        pos2(x, y) * (scale / self.pixels_per_point)
    }
}

struct Shared<P> {
    platform: P,
    windows: BTreeMap<ViewportId, Window>,
    native: HashMap<WindowId, ViewportId>,
    start: Instant,
    /// Device pixels per logical pixel, and egui's points to device pixels at zoom 1.
    scale: f32,
    native_pixels_per_point: f32,
    /// Repaints egui asked for (any thread), by viewport.
    requested: Arc<Mutex<HashMap<ViewportId, Instant>>>,
    /// Passes due, by the viewport whose pass it is (the root or a deferred one).
    due: HashMap<ViewportId, Instant>,
    /// Textures freed by passes, freed once the outermost pass has drawn.
    freed: Vec<egui::TextureId>,
    finished: Finished,
}

/// The windows of every viewport. See the module documentation.
pub struct Viewports<P: Platform> {
    shared: Rc<RefCell<Shared<P>>>,
    ctx: Context,
}

impl<P: Platform> Viewports<P> {
    /// Drives `ctx`'s viewports in native windows; the root viewport is in `root`, which the host
    /// opened. `native_pixels_per_point` is egui's points to device pixels at zoom 1, `scale`
    /// device pixels per logical pixel.
    pub fn new(ctx: &Context, platform: P, root: WindowId, native_pixels_per_point: f32, scale: f32) -> Self {
        ctx.set_embed_viewports(false);
        let requested: Arc<Mutex<HashMap<ViewportId, Instant>>> = Arc::default();
        let wake = platform.waker();
        let asked = requested.clone();
        ctx.set_request_repaint_callback(move |info| {
            let at = Instant::now() + info.delay;
            let mut asked = asked.lock().unwrap_or_else(PoisonError::into_inner);
            let entry = asked.entry(info.viewport_id).or_insert(at);
            *entry = (*entry).min(at);
            drop(asked);
            wake();
        });
        let mut windows = BTreeMap::new();
        windows.insert(
            ViewportId::ROOT,
            Window::new(ViewportIdPair::ROOT, ViewportClass::Root, Some(root), ViewportBuilder::default(), native_pixels_per_point),
        );
        let shared = Rc::new(RefCell::new(Shared {
            platform,
            windows,
            native: std::iter::once((root, ViewportId::ROOT)).collect(),
            start: Instant::now(),
            scale,
            native_pixels_per_point,
            requested,
            due: std::iter::once((ViewportId::ROOT, Instant::now())).collect(),
            freed: Vec::new(),
            finished: Finished::default(),
        }));
        let weak = Rc::downgrade(&shared);
        Context::set_immediate_viewport_renderer(move |ctx, viewport| match weak.upgrade() {
            Some(shared) => render_immediate(&shared, ctx, viewport),
            // Gone (the host is shutting down): egui still needs the callback run.
            None => {
                let ImmediateViewport { ids, mut viewport_ui_cb, .. } = viewport;
                let mut out = ctx.run_ui(RawInput { viewport_id: ids.this, ..RawInput::default() }, |ui| viewport_ui_cb(ui));
                out.textures_delta.clear();
            }
        });
        Self { shared, ctx: ctx.clone() }
    }

    /// The desktop's density changed.
    pub fn set_density(&self, native_pixels_per_point: f32, scale: f32) {
        let mut s = self.shared.borrow_mut();
        s.native_pixels_per_point = native_pixels_per_point;
        s.scale = scale;
        s.due.insert(ViewportId::ROOT, Instant::now());
    }

    /// The native window showing `viewport` (e.g. to put it on another monitor).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn window_of(&self, viewport: ViewportId) -> Option<WindowId> {
        self.shared.borrow().windows.get(&viewport).and_then(|w| w.native)
    }

    /// The viewport native `window` shows.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn viewport_of(&self, window: WindowId) -> Option<ViewportId> {
        self.shared.borrow().native.get(&window).copied()
    }

    /// Input that happened in native `window`; false when no viewport has it.
    pub fn input(&self, window: WindowId, input: WindowInput<'_>) -> bool {
        let mut s = self.shared.borrow_mut();
        let Some(&id) = s.native.get(&window) else { return false };
        let scale = s.scale;
        let paste = match &input {
            WindowInput::Key { pressed: true, .. } => s.platform.clipboard_text(),
            _ => None,
        };
        let Some(w) = s.windows.get_mut(&id) else { return false };
        match input {
            WindowInput::Pointer { x, y, buttons, modifiers } => {
                w.set_modifiers(modifiers);
                let pos = w.pos(x, y, scale);
                w.events.push(Event::PointerMoved(pos));
                let changed = buttons ^ w.buttons;
                for bit in [1, 2, 4, 8, 16] {
                    if changed & bit != 0
                        && let Some(button) = button(bit)
                    {
                        w.events.push(Event::PointerButton { pos, button, pressed: buttons & bit != 0, modifiers: w.modifiers });
                    }
                }
                w.buttons = buttons;
            }
            WindowInput::PointerGone => w.events.push(Event::PointerGone),
            WindowInput::Wheel { dx, dy, modifiers, gesture } => {
                w.set_modifiers(modifiers);
                wheel(w, dx, dy, gesture, self.ctx.options(|o| o.input_options.line_scroll_speed));
            }
            WindowInput::Key { pressed, code, raw_char, modifiers, text } => key(w, pressed, code, raw_char, modifiers, text, paste),
            WindowInput::Modifiers(bits) => w.set_modifiers(bits),
            WindowInput::Focus(focused) => {
                w.focused = focused;
                w.events.push(Event::WindowFocused(focused));
                if !focused {
                    // Keys released elsewhere never come back up.
                    w.set_modifiers(0);
                }
            }
            WindowInput::Close => w.info.events.push(ViewportEvent::Close),
            WindowInput::Dropped { x, y, paths } => {
                let pos = w.pos(x, y, scale);
                w.events.push(Event::PointerMoved(pos));
                w.dropped.extend(paths.into_iter().map(|p| Arc::new(DroppedPath(p)) as egui::DroppedFileHandle));
            }
            WindowInput::Changed => {}
            WindowInput::Preedit { text, selection } => {
                // The clause being converted, from bytes to characters.
                let chars = |b: i32| usize::try_from(b).ok().and_then(|b| text.get(..b)).map(|s| s.chars().count());
                let active_range_chars = match selection.map(|(a, b)| (chars(a), chars(b))) {
                    Some((Some(start), Some(end))) if start <= end => Some(start..end),
                    _ => None,
                };
                w.composing = !text.is_empty();
                w.events.push(Event::Ime(egui::ImeEvent::Preedit { text: text.to_string(), active_range_chars }));
            }
            WindowInput::Commit(text) => {
                w.composing = false;
                w.events.push(Event::Ime(egui::ImeEvent::Commit(text.to_string())));
            }
            WindowInput::ImeStopped => {
                // Stopped without confirming: the pre-edit goes.
                if std::mem::take(&mut w.composing) {
                    w.events.push(Event::Ime(egui::ImeEvent::Preedit { text: String::new(), active_range_chars: None }));
                }
            }
        }
        let owner = s.owner(id);
        s.due.insert(owner, Instant::now());
        true
    }

    /// Run `viewport`'s pass (the root's, or its deferred ancestor's) after `delay`.
    pub fn request(&self, viewport: ViewportId, delay: Duration) {
        let mut s = self.shared.borrow_mut();
        let owner = s.owner(viewport);
        s.schedule(owner, Instant::now() + delay);
    }

    /// When the next pass or presenter chore is due.
    pub fn next_due(&self) -> Option<Instant> {
        let s = self.shared.borrow();
        let asked = s.requested.lock().unwrap_or_else(PoisonError::into_inner).values().min().copied();
        [s.due.values().min().copied(), asked, s.platform.chores_due()].into_iter().flatten().min()
    }

    /// How long the host may wait for input.
    pub fn timeout(&self) -> Option<Duration> {
        self.next_due().map(|at| at.saturating_duration_since(Instant::now()))
    }

    /// The passes due now, the root's first: [`ViewportId::ROOT`] (the host's pass, from
    /// [`Viewports::root_input`]) or deferred viewports ([`Viewports::run_deferred`]).
    pub fn take_due(&self) -> Vec<ViewportId> {
        let mut s = self.shared.borrow_mut();
        let asked: Vec<(ViewportId, Instant)> = s.requested.lock().unwrap_or_else(PoisonError::into_inner).drain().collect();
        for (viewport, at) in asked {
            let owner = s.owner(viewport);
            s.schedule(owner, at);
        }
        let now = Instant::now();
        let mut due: Vec<ViewportId> = s.due.iter().filter(|(_, at)| **at <= now).map(|(id, _)| *id).collect();
        for id in &due {
            s.due.remove(id);
        }
        due.sort_by_key(|id| *id != ViewportId::ROOT);
        due
    }

    /// The presenter's work between frames.
    pub fn chores(&self) {
        let mut s = self.shared.borrow_mut();
        if s.platform.chores_due().is_some_and(|at| at <= Instant::now()) {
            let scale = s.scale;
            s.platform.chores(scale);
        }
    }

    /// The root viewport's input for its pass, or `None` while its window has no size (then the
    /// pass is due again shortly).
    pub fn root_input(&self) -> Option<RawInput> {
        let mut s = self.shared.borrow_mut();
        let input = s.input_for(ViewportId::ROOT);
        if input.is_none() {
            s.schedule(ViewportId::ROOT, Instant::now() + Duration::from_millis(50));
        }
        input
    }

    /// Handle a pass's output: see the module documentation. Call with the root's output after
    /// its pass; the other passes are this module's.
    pub fn finish(&self, viewport: ViewportId, output: FullOutput) -> Finished {
        finish(&self.shared, &self.ctx, viewport, output)
    }

    /// Run a deferred viewport's pass, if it has a window and a callback.
    pub fn run_deferred(&self, viewport: ViewportId) -> Finished {
        let (input, callback) = {
            let mut s = self.shared.borrow_mut();
            let Some(callback) = s.windows.get(&viewport).and_then(|w| w.deferred.clone()) else { return Finished::default() };
            let input = s.input_for(viewport).unwrap_or_else(|| RawInput { viewport_id: viewport, ..RawInput::default() });
            (input, callback)
        };
        // No borrow is held while the app's code runs: it may show immediate viewports.
        let output = self.ctx.run_ui(input, |ui| callback(ui));
        finish(&self.shared, &self.ctx, viewport, output)
    }
}

impl<P: Platform> Drop for Viewports<P> {
    /// The viewports' windows close (the root's is the host's).
    fn drop(&mut self) {
        let mut s = self.shared.borrow_mut();
        let children: Vec<WindowId> = s.windows.iter().filter(|(id, _)| **id != ViewportId::ROOT).filter_map(|(_, w)| w.native).collect();
        for window in children {
            s.platform.close(window);
        }
    }
}

impl<P: Platform> Shared<P> {
    /// Whose pass draws `viewport`: its own for the root and deferred viewports, its parent's for
    /// an immediate one.
    fn owner(&self, mut viewport: ViewportId) -> ViewportId {
        for _ in 0..64 {
            match self.windows.get(&viewport) {
                Some(w) if viewport != ViewportId::ROOT && w.class != ViewportClass::Deferred => viewport = w.ids.parent,
                Some(_) => return viewport,
                None => return ViewportId::ROOT,
            }
        }
        ViewportId::ROOT
    }

    fn schedule(&mut self, owner: ViewportId, at: Instant) {
        let entry = self.due.entry(owner).or_insert(at);
        *entry = (*entry).min(at);
    }

    /// Points to logical pixels for `viewport`.
    fn points_to_logical(&self, viewport: ViewportId) -> f32 {
        let ppp = self.windows.get(&viewport).map_or(self.native_pixels_per_point, |w| w.pixels_per_point);
        ppp / self.scale
    }

    /// The input for `viewport`'s pass: its window's state, its queued events, and every
    /// viewport's info. `None` when its window has no size.
    fn input_for(&mut self, viewport: ViewportId) -> Option<RawInput> {
        let scale = self.scale;
        let native_ppp = self.native_pixels_per_point;
        let max_texture_side = self.platform.max_texture_side();
        let dark = self.platform.dark();
        let time = self.start.elapsed().as_secs_f64();
        let native = self.windows.get(&viewport)?.native;
        let info = native.map(|n| self.platform.info(n));
        let w = self.windows.get_mut(&viewport)?;
        let k = scale / w.pixels_per_point;
        if let Some(info) = info {
            let rect = |r: [f32; 4]| egui::Rect::from_min_max(pos2(r[0], r[1]) * k, pos2(r[2], r[3]) * k);
            w.info.native_pixels_per_point = Some(native_ppp);
            w.info.monitor_size =
                (info.monitor[2] > info.monitor[0]).then(|| vec2(info.monitor[2] - info.monitor[0], info.monitor[3] - info.monitor[1]) * k);
            w.info.inner_rect = Some(rect(info.inner));
            w.info.outer_rect = Some(rect(info.outer));
            w.info.minimized = Some(info.minimized);
            w.info.maximized = Some(false);
            w.info.fullscreen = Some(info.fullscreen);
            w.info.focused = Some(info.active);
            w.info.occluded = Some(info.hidden);
        }
        let logical = info.map_or([0.0, 0.0], |i| [i.inner[2] - i.inner[0], i.inner[3] - i.inner[1]]);
        let size = logical.map(|v| (v * scale).round().max(0.0) as u32);
        w.size = size;
        let available = size[0] >= 1 && size[1] >= 1;
        // A touchpad gesture that ended in the last pass's events ends in this one.
        if available && let Some(modifiers) = w.wheel_end_next.take() {
            w.events.insert(0, gesture_end(modifiers));
        }
        let end_next = available.then(|| w.wheel_end.take()).flatten();
        w.wheel_end_next = end_next;
        let input = RawInput {
            viewport_id: viewport,
            viewports: std::iter::empty().collect(),
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(size[0] as f32, size[1] as f32) / w.pixels_per_point)),
            max_texture_side: Some(max_texture_side),
            time: Some(time),
            predicted_dt: 1.0 / 60.0,
            events: if available { std::mem::take(&mut w.events) } else { Vec::new() },
            dropped_files: if available { std::mem::take(&mut w.dropped) } else { Vec::new() },
            focused: w.focused,
            system_theme: Some(if dark { Theme::Dark } else { Theme::Light }),
            ..RawInput::default()
        };
        if !available && viewport == ViewportId::ROOT {
            return None;
        }
        if end_next.is_some() {
            let owner = self.owner(viewport);
            self.due.insert(owner, Instant::now());
        }
        let mut input = input;
        input.viewports = self.windows.iter().map(|(id, w)| (*id, w.info.clone())).collect();
        // The events are this pass's now.
        if let Some(w) = self.windows.get_mut(&viewport) {
            w.info.events.clear();
        }
        Some(input)
    }

    /// Makes sure `ids.this` has a window: a new one from `builder`, or the existing one changed
    /// to match it.
    fn ensure(
        &mut self,
        ids: ViewportIdPair,
        class: ViewportClass,
        builder: ViewportBuilder,
        deferred: Option<Arc<DeferredViewportUiCallback>>,
        zoom: f32,
    ) {
        if let Some(w) = self.windows.get_mut(&ids.this) {
            if ids.this != ViewportId::ROOT {
                w.class = class;
                w.ids.parent = ids.parent;
                w.info.parent = Some(ids.parent);
            }
            if class == ViewportClass::Deferred {
                w.deferred = deferred;
            }
            let (commands, _recreate) = w.builder.patch(builder);
            for command in commands {
                self.command(ids.this, command);
            }
            return;
        }
        let ppp = self.native_pixels_per_point * zoom;
        let spec = spec(&builder, ppp / self.scale);
        let native = self.platform.open(&spec);
        if native.is_none() {
            log::warn!("couldn't open a window for viewport {:?} ({:?})", ids.this, builder.title);
        }
        let mut w = Window::new(ids, class, native, builder, ppp);
        w.info.title.clone_from(&w.builder.title);
        if class == ViewportClass::Deferred {
            w.deferred = deferred;
        }
        if let Some(n) = native {
            self.native.insert(n, ids.this);
        }
        self.windows.insert(ids.this, w);
        if class == ViewportClass::Deferred {
            self.due.insert(ids.this, Instant::now());
        }
    }

    /// A viewport command for `viewport`'s window.
    fn command(&mut self, viewport: ViewportId, command: ViewportCommand) {
        let k = self.points_to_logical(viewport);
        let owner = self.owner(viewport);
        let paste = matches!(command, ViewportCommand::RequestPaste).then(|| self.platform.clipboard_text()).flatten();
        let Some(w) = self.windows.get_mut(&viewport) else { return };
        let native = w.native;
        let send = |platform: &mut P, c: NativeCommand| {
            if let Some(n) = native {
                platform.command(n, c);
            }
        };
        let size = |v: egui::Vec2| [v.x * k, v.y * k];
        match command {
            ViewportCommand::Close if viewport == ViewportId::ROOT => self.finished.close_root = true,
            ViewportCommand::Close => {
                w.info.events.push(ViewportEvent::Close);
                self.due.insert(owner, Instant::now());
            }
            ViewportCommand::Title(t) => {
                // As eframe: the builder stays as it was given, so the title holds until the
                // builder's own changes.
                w.info.title = Some(t.clone());
                send(&mut self.platform, NativeCommand::Title(t));
            }
            ViewportCommand::Visible(v) => send(&mut self.platform, NativeCommand::Show(v)),
            ViewportCommand::StartDrag => send(&mut self.platform, NativeCommand::StartDrag),
            ViewportCommand::OuterPosition(p) => send(&mut self.platform, NativeCommand::MoveTo([p.x * k, p.y * k])),
            ViewportCommand::InnerSize(s) => send(&mut self.platform, NativeCommand::ResizeTo(size(s))),
            ViewportCommand::MinInnerSize(s) => send(&mut self.platform, NativeCommand::MinSize(size(s))),
            ViewportCommand::MaxInnerSize(s) => send(&mut self.platform, NativeCommand::MaxSize(s.is_finite().then(|| size(s)))),
            ViewportCommand::BeginResize(direction) => send(&mut self.platform, NativeCommand::BeginResize(resize_edges(direction))),
            ViewportCommand::Resizable(r) => send(&mut self.platform, NativeCommand::Resizable(r)),
            ViewportCommand::EnableButtons { close, minimized, maximize } => {
                send(&mut self.platform, NativeCommand::Buttons([close, minimized, maximize]))
            }
            ViewportCommand::Minimized(m) => send(&mut self.platform, NativeCommand::Minimize(m)),
            // Haiku's zoom toggles, and doesn't say whether a window is zoomed.
            ViewportCommand::Maximized(true) => send(&mut self.platform, NativeCommand::Zoom),
            ViewportCommand::Fullscreen(f) => send(&mut self.platform, NativeCommand::Fullscreen(f)),
            ViewportCommand::Decorations(d) => send(&mut self.platform, NativeCommand::Decorations(d)),
            ViewportCommand::WindowLevel(level) => send(&mut self.platform, NativeCommand::Floating(level == WindowLevel::AlwaysOnTop)),
            ViewportCommand::Focus => send(&mut self.platform, NativeCommand::Activate),
            ViewportCommand::Screenshot(callback) => {
                w.screenshots.push(callback);
                self.due.insert(owner, Instant::now());
            }
            ViewportCommand::RequestCut => {
                w.events.push(Event::Cut);
                self.due.insert(owner, Instant::now());
            }
            ViewportCommand::RequestCopy => {
                w.events.push(Event::Copy);
                self.due.insert(owner, Instant::now());
            }
            ViewportCommand::RequestPaste => {
                if let Some(t) = paste {
                    w.events.push(Event::Paste(t.replace("\r\n", "\n")));
                    self.due.insert(owner, Instant::now());
                }
            }
            _ => {}
        }
    }
}

/// The immediate viewport renderer: a window for the viewport, its input, its pass (run here,
/// inside its parent's) and its output.
fn render_immediate<P: Platform>(shared: &Rc<RefCell<Shared<P>>>, ctx: &Context, viewport: ImmediateViewport<'_>) {
    let ImmediateViewport { ids, builder, mut viewport_ui_cb } = viewport;
    let input = {
        let mut s = shared.borrow_mut();
        s.ensure(ids, ViewportClass::Immediate, builder, None, ctx.zoom_factor());
        s.input_for(ids.this).unwrap_or_else(|| RawInput { viewport_id: ids.this, ..RawInput::default() })
    };
    // No borrow is held while the app's code runs: it may show immediate viewports of its own.
    let output = ctx.run_ui(input, |ui| viewport_ui_cb(ui));
    finish(shared, ctx, ids.this, output);
}

fn finish<P: Platform>(shared: &Rc<RefCell<Shared<P>>>, ctx: &Context, viewport: ViewportId, output: FullOutput) -> Finished {
    let FullOutput { platform_output, mut textures_delta, shapes, pixels_per_point, viewport_output } = output;
    let mut s = shared.borrow_mut();
    let s = &mut *s;
    s.platform.set_textures(&textures_delta);
    s.freed.extend(textures_delta.free.iter().copied());
    // Applied (freeing waits for the outermost pass).
    textures_delta.clear();
    let native = s.windows.get(&viewport).and_then(|w| w.native);
    if let Some(w) = s.windows.get_mut(&viewport) {
        w.pixels_per_point = pixels_per_point;
    }
    if let Some(n) = native {
        s.platform.set_cursor(n, platform_output.cursor_icon);
        // The input method follows this window's caret, and stops when its editor interrupts
        // the composition or nothing here takes typing any more.
        let ime = platform_output.ime.as_ref();
        let k = pixels_per_point / s.scale;
        let caret = ime.map(|o| o.cursor_rect).filter(|r| r.is_finite()).map(|r| [r.min.x * k, r.min.y * k, r.max.x * k, r.max.y * k]);
        if let Some(w) = s.windows.get_mut(&viewport) {
            if caret != w.ime_caret {
                w.ime_caret = caret;
                s.platform.ime_caret(n, caret);
            }
            if ime.is_none_or(|o| o.should_interrupt_composition) {
                s.platform.ime_stop(n);
                if std::mem::take(&mut w.composing) {
                    w.events.push(Event::Ime(egui::ImeEvent::Preedit { text: String::new(), active_range_chars: None }));
                }
            }
        }
    }
    for command in &platform_output.commands {
        s.platform.output(command);
    }

    // New viewports, builder changes and commands. A deferred viewport gets its window here; an
    // immediate one got it when it was drawn.
    let now = Instant::now();
    let zoom = ctx.zoom_factor();
    let outermost = viewport == ViewportId::ROOT || s.windows.get(&viewport).is_some_and(|w| w.class == ViewportClass::Deferred);
    for (id, out) in &viewport_output {
        let known = s.windows.contains_key(id);
        if known || out.class == ViewportClass::Deferred {
            s.ensure(ViewportIdPair::from_self_and_parent(*id, out.parent), out.class, out.builder.clone(), out.viewport_ui_cb.clone(), zoom);
        }
        for command in out.commands.iter().cloned() {
            s.command(*id, command);
        }
        if outermost && out.repaint_delay < Duration::from_secs(3600) {
            let owner = s.owner(*id);
            s.schedule(owner, now + out.repaint_delay);
        }
    }

    // Draw.
    let scale = s.scale;
    let shown = native.map(|n| s.platform.info(n)).is_some_and(|i| !i.minimized && !i.hidden);
    let (size, screenshots) = match s.windows.get_mut(&viewport) {
        Some(w) => (w.size, std::mem::take(&mut w.screenshots)),
        None => ([0, 0], Vec::new()),
    };
    if let Some(n) = native
        && shown
        && size[0] > 0
        && size[1] > 0
    {
        let primitives = ctx.tessellate(shapes, pixels_per_point);
        let clear = ctx.global_style().visuals.panel_fill;
        s.platform.paint(n, size, &primitives, pixels_per_point, clear, scale);
    }
    for callback in screenshots {
        let image = native.map(|n| s.platform.screenshot(n)).unwrap_or_default();
        callback.complete(Arc::new(image));
    }

    if outermost {
        // Windows whose viewport egui dropped close.
        let gone: Vec<ViewportId> = s.windows.keys().filter(|id| **id != ViewportId::ROOT && !viewport_output.contains_key(*id)).copied().collect();
        for id in gone {
            if let Some(w) = s.windows.remove(&id)
                && let Some(n) = w.native
            {
                s.native.remove(&n);
                s.platform.close(n);
            }
            s.due.remove(&id);
        }
        let freed = std::mem::take(&mut s.freed);
        s.platform.free_textures(&freed);
    }
    std::mem::take(&mut s.finished)
}

/// A new window's spec from its viewport's builder; `k` turns points into logical pixels.
pub fn spec(builder: &ViewportBuilder, k: f32) -> WindowSpec {
    let size = |v: egui::Vec2| [v.x * k, v.y * k];
    WindowSpec {
        title: builder.title.clone().unwrap_or_default(),
        position: builder.position.map(|p| [p.x * k, p.y * k]),
        size: size(builder.inner_size.unwrap_or(vec2(800.0, 600.0))),
        min_size: builder.min_inner_size.map(size),
        max_size: builder.max_inner_size.filter(|s| s.is_finite()).map(size),
        decorations: builder.decorations.unwrap_or(true),
        resizable: builder.resizable.unwrap_or(true),
        visible: builder.visible.unwrap_or(true),
        zoomed: builder.maximized.unwrap_or(false),
        fullscreen: builder.fullscreen.unwrap_or(false),
        floating: builder.window_level == Some(WindowLevel::AlwaysOnTop),
        buttons: [builder.close_button.unwrap_or(true), builder.minimize_button.unwrap_or(true), builder.maximize_button.unwrap_or(true)],
    }
}

fn resize_edges(direction: ResizeDirection) -> u32 {
    use ResizeDirection as D;
    use edges::*;
    match direction {
        D::North => TOP,
        D::South => BOTTOM,
        D::East => RIGHT,
        D::West => LEFT,
        D::NorthEast => TOP | RIGHT,
        D::SouthEast => BOTTOM | RIGHT,
        D::NorthWest => TOP | LEFT,
        D::SouthWest => BOTTOM | LEFT,
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

/// A wheel turn, or a step of an air/OS touchpad's scroll gesture in window `w`: its precise
/// deltas (wheel steps, unrounded) become points at egui's line scroll speed (`line`), with
/// the gesture's phases, so the canvas follows the fingers 1:1 and coasts after they lift, and
/// egui's scroll areas treat it as a trackpad. A wheel's whole steps stay lines.
fn wheel(w: &mut Window, dx: f32, dy: f32, phase: Option<i32>, line: f32) {
    use egui::{MouseWheelSource, MouseWheelUnit, TouchPhase};
    let modifiers = w.modifiers;
    let Some(phase) = phase else {
        // A mouse wheel, or a system without touchpad gestures.
        w.events.push(Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: vec2(-dx, -dy),
            phase: TouchPhase::Move,
            source: MouseWheelSource::Wheel,
            modifiers,
        });
        return;
    };
    let event = |unit, delta, phase| Event::MouseWheel { unit, delta, phase, source: MouseWheelSource::Trackpad, modifiers };
    let delta = vec2(-dx, -dy) * line;
    let delta = if delta.is_finite() { delta } else { egui::Vec2::ZERO };
    match phase {
        gesture::BEGAN => w.events.push(event(MouseWheelUnit::Point, egui::Vec2::ZERO, TouchPhase::Start)),
        gesture::ENDED => {
            // egui drops a gesture's unapplied deltas at its end: one that ends in the same pass
            // as its last movement ends in the next.
            let moved = w.events.iter().any(|e| matches!(e, Event::MouseWheel { source: MouseWheelSource::Trackpad, phase: TouchPhase::Move, .. }));
            if moved {
                w.wheel_end = Some(modifiers);
            } else {
                w.events.push(gesture_end(modifiers));
            }
            return;
        }
        _ => {}
    }
    if delta != egui::Vec2::ZERO {
        w.events.push(event(MouseWheelUnit::Point, delta, TouchPhase::Move));
    }
}

fn gesture_end(modifiers: Modifiers) -> Event {
    Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::Vec2::ZERO,
        phase: egui::TouchPhase::End,
        source: egui::MouseWheelSource::Trackpad,
        modifiers,
    }
}

/// A key in window `w`; `paste` is the clipboard's text, for the paste shortcuts.
fn key(w: &mut Window, pressed: bool, code: i32, raw_char: i32, bits: u32, text: &str, paste: Option<String>) {
    w.set_modifiers(bits);
    let (logical, physical) = keys::key(code, raw_char, text.as_bytes());
    log::debug!(
        "key {} code {code:#x} raw {raw_char:#x} modifiers {bits:#x} {text:?}: {logical:?} / {physical:?}",
        if pressed { "down" } else { "up" }
    );
    let m = w.modifiers;
    if let Some(active) = logical.or(physical) {
        if pressed {
            // As egui's own integrations do: the clipboard shortcuts become clipboard events.
            if m.command && active == Key::X || m.shift && active == Key::Delete {
                w.events.push(Event::Cut);
                return;
            }
            if m.command && (active == Key::C || active == Key::Insert) {
                w.events.push(Event::Copy);
                return;
            }
            if m.command && active == Key::V || m.shift && active == Key::Insert {
                if let Some(t) = paste {
                    w.events.push(Event::Paste(t.replace("\r\n", "\n")));
                }
                return;
            }
        }
        w.events.push(Event::Key { key: active, physical_key: physical, pressed, repeat: false, modifiers: m });
    }
    if pressed && let Some(t) = keys::text(bits, text) {
        w.events.push(Event::Text(t.to_string()));
    }
}

#[cfg(test)]
mod tests;
