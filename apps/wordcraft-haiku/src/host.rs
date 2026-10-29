//! The native host (`native/host.cpp`): safe wrappers over its C functions. The only module with
//! `unsafe` code; every call here is a plain C call with owned or borrowed buffers.
#![allow(unsafe_code)]

use std::ffi::{CStr, CString, c_char, c_void};

pub use crate::viewports::{NativeCommand, WindowInfo, WindowSpec};

/// An input or window event from the native host. Keep in step with `WcEvent` in `host.cpp`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RawEvent {
    pub kind: i32,
    /// The window it happened in ([`open_window`]'s id); 0 for the application's own events.
    pub window: i32,
    pub buttons: i32,
    pub clicks: i32,
    pub modifiers: u32,
    pub key: i32,
    pub raw_char: i32,
    pub repeat: i32,
    pub id: i32,
    pub x: f32,
    pub y: f32,
    pub dx: f32,
    pub dy: f32,
    pub text_len: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct RawWindowInfo {
    inner: [f32; 4],
    outer: [f32; 4],
    monitor: [f32; 4],
    minimized: i32,
    active: i32,
    zoomed: i32,
    fullscreen: i32,
    hidden: i32,
}

/// The kinds of [`RawEvent`].
pub mod kind {
    pub const MOUSE_MOVED: i32 = 1;
    pub const MOUSE_DOWN: i32 = 2;
    pub const MOUSE_UP: i32 = 3;
    pub const WHEEL: i32 = 4;
    pub const KEY_DOWN: i32 = 5;
    pub const KEY_UP: i32 = 6;
    pub const RESIZED: i32 = 7;
    pub const FOCUS: i32 = 8;
    pub const CLOSE: i32 = 9;
    pub const OPEN_FILES: i32 = 10;
    pub const DROPPED_FILES: i32 = 11;
    pub const POINTER_GONE: i32 = 12;
    pub const QUIT_REQUESTED: i32 = 13;
    pub const DIALOG_ANSWER: i32 = 14;
    pub const MODIFIERS: i32 = 15;
    pub const MOVED: i32 = 16;
    pub const MINIMIZED: i32 = 17;
    pub const COLORS: i32 = 18;
    pub const IME: i32 = 19;
}

/// What an input method's [`kind::IME`] event (its `id`) says: the text being composed (the
/// selected clause from byte `key` to `raw_char`, or -1), the confirmed text, or that the input
/// method stopped.
pub mod ime {
    pub const PREEDIT: i32 = 1;
    pub const COMMIT: i32 = 2;
    pub const STOPPED: i32 = 3;
}

unsafe extern "C" {
    fn wc_host_init(signature: *const c_char) -> i32;
    fn wc_host_open_window(title: *const c_char, x: f32, y: f32, w: f32, h: f32, min_w: f32, min_h: f32, max_w: f32, max_h: f32, flags: i32) -> i32;
    fn wc_host_close_window(window: i32);
    fn wc_host_run();
    fn wc_host_quit();
    fn wc_host_wait(timeout_us: i64) -> i32;
    fn wc_host_wake();
    fn wc_host_next_event(event: *mut RawEvent) -> i32;
    fn wc_host_event_text() -> *const c_char;
    fn wc_host_present(window: i32, pixels: *const u32, w: i32, h: i32, x0: i32, y0: i32, x1: i32, y1: i32, scale: f32);
    fn wc_host_present_rects(window: i32, pixels: *const u32, w: i32, h: i32, rects: *const i32, count: i32, scale: f32);
    fn wc_host_direct_sink(window: i32, sink: Option<unsafe extern "C" fn(i32, *const RawDirect)>);
    fn wc_host_window_info(window: i32, info: *mut RawWindowInfo);
    fn wc_host_set_title(window: i32, title: *const c_char);
    fn wc_host_window_command(window: i32, command: i32, a: f32, b: f32);
    fn wc_host_set_cursor(window: i32, id: i32);
    fn wc_host_modifiers() -> u32;
    fn wc_host_ui_scale() -> f32;
    fn wc_host_device_scale() -> f32;
    fn wc_host_dark() -> i32;
    fn wc_host_locale(languages: *mut *mut c_char) -> *mut c_char;
    fn wc_free(data: *mut c_void);
    fn wc_clipboard_get(mime: *const c_char, len: *mut usize) -> *mut c_void;
    fn wc_clipboard_formats() -> *mut c_char;
    fn wc_clipboard_set(text: *const c_char, text_len: usize, html: *const c_char, html_len: usize, mathml: *const c_char, mathml_len: usize) -> i32;
    fn wc_clipboard_set_picture(mime: *const c_char, data: *const c_void, len: usize) -> i32;
    fn wc_clipboard_bitmap(w: *mut i32, h: *mut i32) -> *mut c_void;
    fn wc_host_file_panel(id: i32, mode: i32, name: *const c_char, extensions: *const c_char, title: *const c_char, directory: *const c_char) -> i32;
    fn wc_host_open_url(url: *const c_char) -> i32;
    fn wc_host_ime_caret(window: i32, known: i32, left: f32, top: f32, right: f32, bottom: f32);
    fn wc_host_ime_stop(window: i32);
    fn wc_host_make_default(signature: *const c_char, types: *const c_char) -> i32;
    fn wc_host_alert(title: *const c_char, text: *const c_char);
    fn wc_host_type_file(path: *const c_char) -> i32;
    fn wc_host_claim_types(signature: *const c_char, types: *const c_char) -> i32;
}

/// Text for C: interior NULs (never in what we pass) are dropped rather than failing.
fn c(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

/// Takes a `malloc`'d C string from the host.
fn take_string(p: *mut c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: the host returns NUL-terminated strings from strdup.
    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    // SAFETY: allocated by the host's malloc.
    unsafe { wc_free(p.cast()) };
    s
}

/// Takes `malloc`'d bytes from the host.
fn take_bytes(p: *mut c_void, len: usize) -> Option<Vec<u8>> {
    if p.is_null() {
        return None;
    }
    // SAFETY: the host allocated `len` bytes at `p`.
    let v = unsafe { std::slice::from_raw_parts(p.cast::<u8>(), len) }.to_vec();
    // SAFETY: allocated by the host's malloc.
    unsafe { wc_free(p) };
    Some(v)
}

/// How [`init`] went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Launch {
    /// This is the running copy.
    Started,
    /// A copy started from the same file is already running (`B_SINGLE_LAUNCH`); the system
    /// handed it this launch's arguments, and this process only has to end.
    HandedOver,
}

/// Creates the BApplication. Call once, on the main thread, before anything else here.
pub fn init(signature: &str) -> Result<Launch, String> {
    let sig = c(signature);
    // SAFETY: a valid C string.
    match unsafe { wc_host_init(sig.as_ptr()) } {
        0 => Ok(Launch::Started),
        1 => Ok(Launch::HandedOver),
        e => Err(format!("the application couldn't start ({e})")),
    }
}

/// Opens a window (logical pixels; no position centres it) and returns its id.
pub fn open_window(spec: &WindowSpec) -> Option<i32> {
    let title = c(&spec.title);
    let [x, y] = spec.position.unwrap_or([-1.0, -1.0]);
    let [min_w, min_h] = spec.min_size.unwrap_or([1.0, 1.0]);
    let [max_w, max_h] = spec.max_size.unwrap_or([0.0, 0.0]);
    let flag = |on: bool, bit: i32| if on { bit } else { 0 };
    let flags = flag(!spec.decorations, 1)
        | flag(!spec.resizable, 2)
        | flag(!spec.visible, 4)
        | flag(spec.zoomed, 8)
        | flag(!spec.buttons[0], 16)
        | flag(!spec.buttons[1], 32)
        | flag(!spec.buttons[2], 64)
        | flag(spec.floating, 128)
        | flag(spec.fullscreen, 256);
    // SAFETY: a valid C string; the host copies it.
    let id = unsafe { wc_host_open_window(title.as_ptr(), x, y, spec.size[0], spec.size[1], min_w, min_h, max_w, max_h, flags) };
    (id > 0).then_some(id)
}

/// Closes a window [`open_window`] opened. Unset its direct sink first ([`Native`] does).
pub fn close_window(window: i32) {
    // SAFETY: no pointers; unknown ids are ignored.
    unsafe { wc_host_close_window(window) }
}

/// Runs the application's message loop on this (the main) thread until [`quit`].
pub fn run() {
    // SAFETY: called once, after `init`.
    unsafe { wc_host_run() }
}

/// Ends the message loop (the windows close and [`run`] returns).
pub fn quit() {
    // SAFETY: no arguments.
    unsafe { wc_host_quit() }
}

/// Waits for an event, a [`wake`] or the timeout (`None`: no timeout).
pub fn wait(timeout: Option<std::time::Duration>) {
    let us = timeout.map_or(-1, |t| i64::try_from(t.as_micros()).unwrap_or(i64::MAX));
    // SAFETY: no pointers.
    unsafe { wc_host_wait(us) };
}

/// Ends a [`wait`] early; callable from any thread.
pub fn wake() {
    // SAFETY: no arguments.
    unsafe { wc_host_wake() }
}

/// The next queued event and its text.
pub fn next_event() -> Option<(RawEvent, String)> {
    let mut ev = RawEvent::default();
    // SAFETY: `ev` is a valid, writable `WcEvent`.
    if unsafe { wc_host_next_event(&mut ev) } == 0 {
        return None;
    }
    let text = if ev.text_len == 0 {
        String::new()
    } else {
        // SAFETY: the text stays valid until the next `wc_host_next_event`; it holds `text_len` bytes.
        let bytes = unsafe { std::slice::from_raw_parts(wc_host_event_text().cast::<u8>(), ev.text_len as usize) };
        String::from_utf8_lossy(bytes).into_owned()
    };
    Some((ev, text))
}

/// Shows `pixels` (`width` × `height` device pixels, 0xAARRGGBB, `scale` per logical pixel) in
/// `window`; only the `dirty` rectangle ([x0, y0, x1, y1), pixels) is copied and redrawn.
pub fn present(window: i32, pixels: &[u32], width: usize, height: usize, dirty: [usize; 4], scale: f32) {
    if pixels.len() < width.saturating_mul(height) {
        return;
    }
    let to_i32 = |v: usize| i32::try_from(v).unwrap_or(i32::MAX);
    // SAFETY: `pixels` holds width × height values; the host only reads them.
    unsafe {
        wc_host_present(
            window,
            pixels.as_ptr(),
            to_i32(width),
            to_i32(height),
            to_i32(dirty[0]),
            to_i32(dirty[1]),
            to_i32(dirty[2]),
            to_i32(dirty[3]),
            scale,
        )
    }
}

/// The window's direct connection as `host.cpp` reports it (`WcDirect`): where its content
/// is in the frame buffer, what of it is visible, or `connected` 0 when it may not draw there.
#[repr(C)]
struct RawDirect {
    connected: i32,
    /// Window bounds, frame buffer pixels, edges included.
    bounds: [i32; 4],
    bytes_per_row: u32,
    rect_count: u32,
    rects: *const i32,
}

type Links = std::collections::HashMap<i32, std::sync::Arc<crate::present::DirectLink>>;

/// Where each window's direct connection goes (its presenter's link).
static DIRECT_LINKS: std::sync::Mutex<Option<Links>> = std::sync::Mutex::new(None);

fn direct_links() -> std::sync::MutexGuard<'static, Option<Links>> {
    DIRECT_LINKS.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A window's direct daemon thread, with `host.cpp`'s connection lock held.
unsafe extern "C" fn direct_connected(window: i32, raw: *const RawDirect) {
    let link = direct_links().as_ref().and_then(|l| l.get(&window)).cloned();
    let Some(link) = link else { return };
    // SAFETY: host.cpp passes a valid report whose rectangles live for the call.
    let raw = unsafe { &*raw };
    if raw.connected == 0 || raw.rects.is_null() {
        link.connected(None);
        return;
    }
    // SAFETY: `rect_count` rectangles of four values each.
    let flat = unsafe { std::slice::from_raw_parts(raw.rects, raw.rect_count as usize * 4) };
    let rects: Vec<[i32; 4]> = flat.as_chunks::<4>().0.to_vec();
    let [left, top, right, bottom] = raw.bounds;
    let size = [u32::try_from(right - left + 1).unwrap_or(0), u32::try_from(bottom - top + 1).unwrap_or(0)];
    link.connected(Some(crate::present::Connection { origin: [left, top], size, rects: &rects, bytes_per_row: raw.bytes_per_row }));
}

/// A native window, for its presenter (`present.rs`).
#[derive(Clone, Copy, Debug)]
pub struct Native(pub i32);

impl crate::present::Native for Native {
    fn show(&self, pixels: &[u32], size: [u32; 2], rects: &[[u32; 4]], scale: f32) {
        let [w, h] = size;
        if pixels.len() < (w as usize).saturating_mul(h as usize) || rects.is_empty() {
            return;
        }
        let flat: Vec<i32> = rects.iter().flatten().map(|&v| i32::try_from(v).unwrap_or(i32::MAX)).collect();
        let n = |v: usize| i32::try_from(v).unwrap_or(i32::MAX);
        // SAFETY: `pixels` holds w × h values and `flat` the rectangles; the host only reads them.
        unsafe { wc_host_present_rects(self.0, pixels.as_ptr(), n(w as usize), n(h as usize), flat.as_ptr(), n(rects.len()), scale) }
    }

    fn set_direct_link(&self, link: Option<std::sync::Arc<crate::present::DirectLink>>) {
        // host.cpp calls `direct_connected` with its lock held; take that lock first when
        // unsetting (it waits for a report in progress), and set ours first when setting (the
        // latest report is replayed at once).
        match link {
            Some(link) => {
                direct_links().get_or_insert_default().insert(self.0, link);
                // SAFETY: a function with the expected signature.
                unsafe { wc_host_direct_sink(self.0, Some(direct_connected)) }
            }
            None => {
                // SAFETY: unsets it.
                unsafe { wc_host_direct_sink(self.0, None) }
                if let Some(links) = direct_links().as_mut() {
                    links.remove(&self.0);
                }
            }
        }
    }

    fn waker(&self) -> fn() {
        wake
    }
}

/// Where a window is (logical screen pixels) and its state; all zero for a closed one.
pub fn window_info(window: i32) -> WindowInfo {
    let mut raw = RawWindowInfo::default();
    // SAFETY: a writable `WcWindowInfo`.
    unsafe { wc_host_window_info(window, &mut raw) };
    WindowInfo {
        inner: raw.inner,
        outer: raw.outer,
        monitor: raw.monitor,
        minimized: raw.minimized != 0,
        active: raw.active != 0,
        fullscreen: raw.fullscreen != 0,
        hidden: raw.hidden != 0,
    }
}

pub fn set_title(window: i32, title: &str) {
    let t = c(title);
    // SAFETY: a valid C string; the host copies it.
    unsafe { wc_host_set_title(window, t.as_ptr()) }
}

/// A window command (`wc_host_window_command`), in logical pixels.
pub fn window_command(window: i32, cmd: &NativeCommand) {
    let on = |b: bool| if b { 1.0 } else { 0.0 };
    let (id, a, b) = match cmd {
        NativeCommand::Title(t) => return set_title(window, t),
        NativeCommand::MoveTo([x, y]) => (1, *x, *y),
        NativeCommand::ResizeTo([w, h]) => (2, *w, *h),
        NativeCommand::Minimize(m) => (3, on(*m), 0.0),
        NativeCommand::Zoom => (4, 1.0, 0.0),
        NativeCommand::Activate => (5, 0.0, 0.0),
        NativeCommand::MinSize([w, h]) => (6, *w, *h),
        NativeCommand::Show(v) => (7, on(*v), 0.0),
        NativeCommand::Decorations(d) => (8, on(*d), 0.0),
        NativeCommand::Resizable(r) => (9, on(*r), 0.0),
        NativeCommand::Floating(f) => (10, on(*f), 0.0),
        NativeCommand::Fullscreen(f) => (11, on(*f), 0.0),
        NativeCommand::StartDrag => (12, 0.0, 0.0),
        NativeCommand::BeginResize(edges) => (13, *edges as f32, 0.0),
        NativeCommand::MaxSize(size) => {
            let [w, h] = size.unwrap_or([0.0, 0.0]);
            (14, w, h)
        }
        NativeCommand::Buttons([close, minimize, zoom]) => (15, on(*close) + 2.0 * on(*minimize) + 4.0 * on(*zoom), 0.0),
    };
    // SAFETY: no pointers.
    unsafe { wc_host_window_command(window, id, a, b) }
}

/// One of Haiku's `BCursorID`s, for a window's canvas.
pub fn set_cursor(window: i32, id: i32) {
    // SAFETY: no pointers; unknown ids fall back to the default cursor.
    unsafe { wc_host_set_cursor(window, id) }
}

/// Haiku's modifier key bits (`modifiers()`).
pub fn modifiers() -> u32 {
    // SAFETY: no arguments.
    unsafe { wc_host_modifiers() }
}

/// The interface scale: the system font size against 12 pt.
pub fn ui_scale() -> f32 {
    // SAFETY: no arguments.
    unsafe { wc_host_ui_scale() }
}

/// Frame buffer pixels per logical pixel: air/OS draws the desktop at its monitors' density
/// (2 at 200 %); 1 elsewhere. `WORDCRAFT_DEVICE_SCALE` overrides it.
pub fn device_scale() -> f32 {
    if let Some(s) = std::env::var("WORDCRAFT_DEVICE_SCALE").ok().and_then(|s| s.parse::<f32>().ok()).filter(|s| (0.5..=4.0).contains(s)) {
        return s;
    }
    // SAFETY: no arguments.
    let s = unsafe { wc_host_device_scale() };
    if s.is_finite() && (1.0..=4.0).contains(&s) { s } else { 1.0 }
}

/// Whether the system's panel colour is dark.
pub fn dark() -> bool {
    // SAFETY: no arguments.
    unsafe { wc_host_dark() != 0 }
}

/// The system's formatting conventions (`en_US`) and preferred languages (`en`, `de`…).
pub fn locale() -> (String, Vec<String>) {
    let mut langs: *mut c_char = std::ptr::null_mut();
    // SAFETY: a writable pointer; both strings come back from strdup.
    let id = take_string(unsafe { wc_host_locale(&mut langs) });
    let langs = take_string(langs).lines().map(str::to_string).filter(|l| !l.is_empty()).collect();
    (id, langs)
}

/// Sets the locale variables the interface reads (`LANG`, `LC_*`) from the system's settings,
/// where the environment doesn't have them (an app launched from Tracker or the Deskbar).
/// Call before any other thread starts.
pub fn export_locale() {
    if std::env::var_os("LANG").is_some() || std::env::var_os("LC_ALL").is_some() {
        return;
    }
    let (conventions, languages) = locale();
    let safe = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '@');
    let lang = languages.first().filter(|l| safe(l)).map(|l| l.replace('-', "_"));
    let lang = match (lang, safe(&conventions)) {
        // "de" with conventions "de_CH": de_CH; otherwise the language alone.
        (Some(l), true) if conventions.starts_with(&format!("{l}_")) => conventions.clone(),
        (Some(l), _) => l,
        (None, true) => conventions.clone(),
        (None, false) => return,
    };
    // SAFETY: called from `main` before any thread is spawned, so nothing reads the environment
    // concurrently.
    unsafe {
        std::env::set_var("LANG", format!("{lang}.UTF-8"));
        if safe(&conventions) {
            for var in ["LC_NUMERIC", "LC_MONETARY", "LC_PAPER", "LC_MEASUREMENT", "LC_TIME"] {
                if std::env::var_os(var).is_none() {
                    std::env::set_var(var, format!("{conventions}.UTF-8"));
                }
            }
        }
        if languages.len() > 1 && std::env::var_os("LANGUAGE").is_none() {
            std::env::set_var("LANGUAGE", languages.iter().filter(|l| safe(l)).map(|l| l.replace('-', "_")).collect::<Vec<_>>().join(":"));
        }
    }
}

/// The clipboard's data of a MIME type.
pub fn clipboard_get(mime: &str) -> Option<Vec<u8>> {
    let m = c(mime);
    let mut len = 0usize;
    // SAFETY: a valid C string and a writable length.
    let p = unsafe { wc_clipboard_get(m.as_ptr(), &mut len) };
    take_bytes(p, len)
}

/// The names of the clipboard's data.
pub fn clipboard_formats() -> Vec<String> {
    // SAFETY: no arguments; the list comes back from strdup.
    take_string(unsafe { wc_clipboard_formats() }).lines().map(str::to_string).filter(|l| !l.is_empty()).collect()
}

/// Replaces the clipboard with `text` and, if given, its HTML and a lone equation's MathML
/// (in the MathML MIME types).
pub fn clipboard_set(text: &str, html: Option<&str>, mathml: Option<&str>) -> bool {
    let raw = |s: Option<&str>| s.map_or((std::ptr::null(), 0), |s| (s.as_ptr().cast::<c_char>(), s.len()));
    let ((h, hl), (m, ml)) = (raw(html), raw(mathml));
    // SAFETY: the pointers and lengths describe live buffers the host copies.
    unsafe { wc_clipboard_set(text.as_ptr().cast(), text.len(), h, hl, m, ml) == 0 }
}

/// Replaces the clipboard with an encoded picture.
pub fn clipboard_set_picture(mime: &str, data: &[u8]) -> bool {
    let m = c(mime);
    // SAFETY: valid buffers the host copies.
    unsafe { wc_clipboard_set_picture(m.as_ptr(), data.as_ptr().cast(), data.len()) == 0 }
}

/// A picture another Haiku program put on the clipboard as a BBitmap: width, height, straight RGBA.
pub fn clipboard_bitmap() -> Option<(usize, usize, Vec<u8>)> {
    let (mut w, mut h) = (0i32, 0i32);
    // SAFETY: two writable ints; the pixels come back malloc'd, w × h × 4 bytes.
    let p = unsafe { wc_clipboard_bitmap(&mut w, &mut h) };
    let (w, h) = (usize::try_from(w).ok()?, usize::try_from(h).ok()?);
    let rgba = take_bytes(p, w.checked_mul(h)?.checked_mul(4)?)?;
    Some((w, h, rgba))
}

/// Which file panel [`file_panel`] shows.
#[derive(Clone, Copy)]
pub enum Panel {
    /// Open one file.
    Open,
    /// Save, suggesting a name.
    Save,
    /// Open any number of files.
    OpenMany,
}

/// Shows an Open panel (offering `extensions`; `*` is any file) or a Save panel (suggesting
/// `name`, starting in `dir` when given); the answer comes back as a [`kind::DIALOG_ANSWER`]
/// event with `id`, the picked paths one per line.
pub fn file_panel(id: i32, panel: Panel, name: &str, extensions: &[&str], title: &str, dir: Option<&str>) {
    let mode = match panel {
        Panel::Open => 0,
        Panel::Save => 1,
        Panel::OpenMany => 2,
    };
    let (n, e, t, d) = (c(name), c(&extensions.join("\n")), c(title), c(dir.unwrap_or("")));
    // SAFETY: valid C strings; the host copies them.
    unsafe { wc_host_file_panel(id, mode, n.as_ptr(), e.as_ptr(), t.as_ptr(), d.as_ptr()) };
}

/// Where the caret is in `window` (view coordinates, logical pixels: left, top, right, bottom)
/// for an input method's candidate window, or `None` when nothing there takes typing.
pub fn ime_caret(window: i32, rect: Option<[f32; 4]>) {
    let [l, t, r, b] = rect.unwrap_or_default();
    // SAFETY: no pointers.
    unsafe { wc_host_ime_caret(window, i32::from(rect.is_some()), l, t, r, b) }
}

/// Ends the input method's composition in `window`, if one is going on.
pub fn ime_stop(window: i32) {
    // SAFETY: no pointers.
    unsafe { wc_host_ime_stop(window) }
}

/// Opens a URL with its preferred application.
pub fn open_url(url: &str) -> bool {
    let u = c(url);
    // SAFETY: a valid C string.
    unsafe { wc_host_open_url(u.as_ptr()) == 0 }
}

/// Makes `signature` the preferred application of the MIME types.
pub fn make_default(signature: &str, types: &[&str]) -> Result<(), String> {
    let (s, t) = (c(signature), c(&types.join("\n")));
    // SAFETY: valid C strings.
    match unsafe { wc_host_make_default(s.as_ptr(), t.as_ptr()) } {
        n if n > 0 => Ok(()),
        _ => Err("the file types couldn't be changed".into()),
    }
}

/// Sets a saved file's MIME type (`BEOS:TYPE`) from its extension and content, if it has none.
pub fn type_file(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(p) = CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: a valid C string.
    unsafe { wc_host_type_file(p.as_ptr()) == 0 }
}

/// Makes `signature` the preferred application of those of the MIME types that have none;
/// how many it claimed.
pub fn claim_types(signature: &str, types: &[&str]) -> i32 {
    let (s, t) = (c(signature), c(&types.join("\n")));
    // SAFETY: valid C strings.
    unsafe { wc_host_claim_types(s.as_ptr(), t.as_ptr()) }
}

/// A modal error alert.
pub fn alert(title: &str, text: &str) {
    let (t, x) = (c(title), c(text));
    // SAFETY: valid C strings; the alert copies them.
    unsafe { wc_host_alert(t.as_ptr(), x.as_ptr()) }
}
