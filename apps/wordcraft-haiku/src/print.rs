//! Printing through the Be print kit (`native/print.cpp`): the printers the print server knows,
//! their drivers' settings, and jobs whose pages are drawn into a `BPrintJob` as bitmaps.
//!
//! The app hands over a PDF (or anything else that can draw pages, [`PageSource`]); each page is
//! rasterised (PDFs with hayro) at the printer's resolution, at most [`MAX_DPI`], placed on the
//! printer's paper at its real size (shrunk when it doesn't fit, turned when a landscape page
//! meets portrait paper) and spooled as the strips of rows that have ink, grey ones with one byte
//! a pixel. The print server then runs the printer's driver on the spool file.
//!
//! Self-contained, so that every Craft host can take it unchanged: copy this file and
//! `native/print.cpp`, rename the `wc_print_` prefix in both, build the C++ file with the host
//! (it needs `libbe`) and add `hayro` to the host's dependencies. The interface is narrow:
//! [`printers`], [`print_pdf`] or [`print`] with a [`PageSource`], and [`Store`] for the
//! settings a printer's setup dialogs leave. An app with a printer list of its own names the
//! printer; one without passes none, and the system's print setup (the print server's window
//! with its printer menu, then that printer's page and job setup) asks for it.
#![allow(unsafe_code)]

use std::path::PathBuf;

/// The highest resolution pages are rasterised at, in dots per inch. Printers that take more
/// get the bitmaps scaled up by their driver: text at 300 dpi prints sharp, and an A4 page
/// stays within 35 MB (a quarter of that in grey) where 600 dpi would need 140 MB per page in
/// memory and in the spool file.
pub const MAX_DPI: f32 = 300.0;

/// The largest page bitmap, in pixels (A2 at 300 dpi); bigger pages are rasterised coarser.
const MAX_PIXELS: f32 = 36_000_000.0;

/// A printer of the print server (a folder in `~/config/settings/printers`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Printer {
    /// The printer's name: its folder's, and what jobs and [`print`] name.
    pub name: String,
    /// Its driver add-on (`PS Compatible`, `PCL6 Compatible`, `Preview`, `Gutenprint`…).
    pub driver: String,
    /// How the driver's output leaves (`USB Port`, `IPP`, `Print To File`…); empty for none.
    pub transport: String,
    /// The print server's default printer (Printers preferences).
    pub default: bool,
    pub comments: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Duplex {
    Off,
    LongEdge,
    ShortEdge,
}

/// What a job asks for beyond the printer's own settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// The job's name in the print server's queue.
    pub title: String,
    pub copies: u32,
    pub collate: bool,
    /// None keeps the printer's setting. Drivers without two-sided printing ignore it.
    pub duplex: Option<Duplex>,
    /// Pages are printed in grey (and the driver is set to monochrome where it can be).
    pub grayscale: bool,
    /// Ask the driver for colour where it has a colour mode (libprint's drivers start in
    /// monochrome), unless its job setup dialog was answered for the printer: that choice stays.
    pub color: bool,
    /// The pages' orientation on paper: `Some(true)` turns portrait pages sideways as well
    /// (landscape pages always print across the paper's long side); None or `Some(false)`
    /// follows each page.
    pub landscape: Option<bool>,
    /// Show the driver's page setup and job setup dialogs first (their answers are kept in the
    /// [`Store`] for the next job), as a Be application's Page Setup and Print do. The job setup's
    /// page range then picks from the pages given, and its copies count. Always so without a
    /// printer.
    pub setup: bool,
    /// The pages to print (0-based), in order; None prints all.
    pub pages: Option<Vec<usize>>,
}

impl Default for Options {
    fn default() -> Self {
        Options { title: String::new(), copies: 1, collate: true, duplex: None, grayscale: false, color: false, landscape: None, setup: false, pages: None }
    }
}

/// Why a job didn't print.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The user cancelled a setup dialog or the job's progress window.
    Cancelled,
    /// The printer's driver has no default settings (it is set up through its dialogs).
    Unsupported,
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Cancelled => f.write_str("printing was cancelled"),
            Error::Unsupported => f.write_str("the printer's driver has no default settings"),
            Error::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

fn failed(message: impl Into<String>) -> Error {
    Error::Failed(message.into())
}

/// An opaque page bitmap: RGBA, 4 bytes a pixel, rows `width * 4` bytes long.
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// The pages of a document, rasterised on demand.
pub trait PageSource {
    fn count(&self) -> usize;
    /// The page's size in points (1/72 inch), as it reads.
    fn size(&self, page: usize) -> (f32, f32);
    /// The page drawn on white, `scale` pixels a point, `width` x `height` pixels.
    fn render(&mut self, page: usize, scale: f32, width: usize, height: usize) -> Result<Image, Error>;
}

/// The pages of a PDF, drawn by hayro.
pub struct PdfPages {
    pdf: hayro::hayro_syntax::Pdf,
    sizes: Vec<(f32, f32)>,
}

impl PdfPages {
    pub fn new(bytes: Vec<u8>) -> Result<Self, Error> {
        let pdf = hayro::hayro_syntax::Pdf::new(bytes).map_err(|e| failed(format!("the PDF can't be read: {e:?}")))?;
        let sizes = pdf.pages().iter().map(|p| p.render_dimensions()).collect();
        Ok(PdfPages { pdf, sizes })
    }

    /// The PDF's title, when it has one.
    pub fn title(&self) -> Option<String> {
        let title = self.pdf.metadata().title.as_ref()?;
        let text = if let Some(utf16) = title.strip_prefix(&[0xfe, 0xff]) {
            let units: Vec<u16> = utf16.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
            String::from_utf16_lossy(&units)
        } else {
            title.iter().map(|&b| char::from(b)).collect()
        };
        let text = text.trim().to_owned();
        (!text.is_empty()).then_some(text)
    }
}

impl PageSource for PdfPages {
    fn count(&self) -> usize {
        self.sizes.len()
    }

    fn size(&self, page: usize) -> (f32, f32) {
        self.sizes.get(page).copied().unwrap_or((612.0, 792.0))
    }

    fn render(&mut self, page: usize, scale: f32, width: usize, height: usize) -> Result<Image, Error> {
        let pages = self.pdf.pages();
        let page = pages.get(page).ok_or_else(|| failed("the PDF has fewer pages"))?;
        let (w, h) = (u16::try_from(width).map_err(|_| failed("page too large"))?, u16::try_from(height).map_err(|_| failed("page too large"))?);
        let settings = hayro::RenderSettings {
            x_scale: scale,
            y_scale: scale,
            width: Some(w),
            height: Some(h),
            bg_color: hayro::vello_cpu::color::palette::css::WHITE,
        };
        let cache = hayro::RenderCache::new();
        let pixmap = hayro::render(page, &cache, &hayro::hayro_interpret::InterpreterSettings::default(), &settings);
        // Drawn on opaque white: premultiplied is the same as straight.
        Ok(Image { width: usize::from(pixmap.width()), height: usize::from(pixmap.height()), rgba: pixmap.data_as_u8_slice().to_vec() })
    }
}

/// The settings each printer's setup dialogs left, one file per printer (a flattened
/// `BMessage`), so the paper, resolution and driver options chosen once stay chosen.
#[derive(Clone, Debug, Default)]
pub struct Store {
    dir: Option<PathBuf>,
}

impl Store {
    /// Settings kept in `dir` (created when first needed); None keeps none.
    pub fn new(dir: Option<PathBuf>) -> Self {
        Store { dir }
    }

    fn file(&self, printer: &str) -> Option<PathBuf> {
        let name: String = printer.chars().map(|c| if c == '/' || c.is_control() { '_' } else { c }).collect();
        self.dir.as_ref().map(|d| d.join(format!("{}.settings", name.trim_start_matches('.'))))
    }

    pub fn load(&self, printer: &str) -> Option<Vec<u8>> {
        std::fs::read(self.file(printer)?).ok().filter(|b| !b.is_empty() && b.len() < 1 << 20)
    }

    pub fn save(&self, printer: &str, settings: &[u8]) {
        let Some(file) = self.file(printer) else { return };
        if let Some(dir) = file.parent()
            && let Err(e) = std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&file, settings))
        {
            log::warn!("keeping the printer's settings in {}: {e}", file.display());
        }
    }
}

/// Where a page goes on the paper, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// The page is turned a quarter counter-clockwise (its top to the paper's left edge).
    pub rotate: bool,
    /// Its size on paper (after turning and shrinking).
    pub width: f32,
    pub height: f32,
    /// How much it was shrunk to fit (1 = real size).
    pub shrink: f32,
    /// Its top left from the printable rectangle's (where `BPrintJob::DrawView` measures from).
    pub x: f32,
    pub y: f32,
}

/// A page of `page` points on `paper` (left, top, right, bottom points) of which `printable`
/// prints: at its real size, centred, shrunk only when it is bigger than the paper. A landscape
/// page (or any page, with `landscape`) on portrait paper is turned, and the other way round.
pub fn place(page: (f32, f32), paper: [f32; 4], printable: [f32; 4], landscape: bool) -> Placement {
    let (pw, ph) = (page.0.max(1.0), page.1.max(1.0));
    let (paper_w, paper_h) = ((paper[2] - paper[0]).max(1.0), (paper[3] - paper[1]).max(1.0));
    let page_landscape = pw > ph || (landscape && pw < ph);
    let rotate = page_landscape != (paper_w > paper_h) && (pw - ph).abs() > 0.5;
    let (w, h) = if rotate { (ph, pw) } else { (pw, ph) };
    let shrink = (paper_w / w).min(paper_h / h).min(1.0);
    let (w, h) = (w * shrink, h * shrink);
    Placement {
        rotate,
        width: w,
        height: h,
        shrink,
        x: paper[0] + (paper_w - w) / 2.0 - printable[0],
        y: paper[1] + (paper_h - h) / 2.0 - printable[1],
    }
}

/// Pixels a point for a page drawn `width` x `height` points on a printer of `dpi`.
pub fn raster_scale(dpi: f32, width: f32, height: f32) -> f32 {
    let dpi = if dpi.is_finite() && dpi >= 36.0 { dpi.min(MAX_DPI) } else { MAX_DPI };
    let mut scale = dpi / 72.0;
    let pixels = width * height * scale * scale;
    if pixels > MAX_PIXELS {
        scale *= (MAX_PIXELS / pixels).sqrt();
    }
    // hayro's bitmaps are at most 65535 pixels a side.
    scale.min(65_000.0 / width.max(height).max(1.0))
}

/// `image` turned a quarter counter-clockwise.
pub fn rotate_ccw(image: &Image) -> Image {
    let (w, h) = (image.width, image.height);
    let mut out = vec![255u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            // Source (x, y) lands at (y, w - 1 - x) of the h-wide result.
            let (src, dst) = ((y * w + x) * 4, ((w - 1 - x) * h + y) * 4);
            if let (Some(s), Some(d)) = (image.rgba.get(src..src + 4), out.get_mut(dst..dst + 4)) {
                d.copy_from_slice(s);
            }
        }
    }
    Image { width: h, height: w, rgba: out }
}

/// A strip of a page bitmap to spool: the rows `y..y + height` and the columns that have ink,
/// in the Be API's layout (`B_RGB32`: B, G, R, A bytes; or `B_GRAY8`).
#[derive(Debug, PartialEq, Eq)]
pub struct Strip {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub gray: bool,
    pub pixels: Vec<u8>,
}

/// Rows this many apart or closer go into one strip: fewer, slightly larger bitmaps.
const STRIP_GAP: usize = 8;

/// The strips of `image` that aren't white paper. Strips meet only across white rows, so a
/// rounding at their edges can't show as a line; grey strips (and every strip with `grayscale`)
/// take one byte a pixel.
pub fn strips(image: &Image, grayscale: bool) -> Vec<Strip> {
    let (w, h) = (image.width, image.height);
    let row = |y: usize| image.rgba.get(y * w * 4..(y + 1) * w * 4).unwrap_or(&[]);
    // Each row's first and last inked column.
    let ink: Vec<Option<(usize, usize)>> = (0..h)
        .map(|y| {
            let r = row(y);
            let inked = |x: &usize| r.get(x * 4..x * 4 + 3).is_some_and(|p| p != [255, 255, 255]);
            let first = (0..w).find(inked)?;
            let last = (first..w).rev().find(inked)?;
            Some((first, last))
        })
        .collect();
    let mut out = Vec::new();
    let mut y = 0;
    while y < h {
        let Some(Some((mut left, mut right))) = ink.get(y).copied() else {
            y += 1;
            continue;
        };
        let top = y;
        let mut bottom = y;
        let mut next = y + 1;
        while next < h && next <= bottom + STRIP_GAP {
            if let Some(Some((l, r))) = ink.get(next).copied() {
                left = left.min(l);
                right = right.max(r);
                bottom = next;
            }
            next += 1;
        }
        let (sw, sh) = (right - left + 1, bottom - top + 1);
        let gray = grayscale
            || (top..=bottom)
                .all(|y| row(y).get(left * 4..(right + 1) * 4).is_some_and(|r| r.as_chunks::<4>().0.iter().all(|p| p[0] == p[1] && p[1] == p[2])));
        let mut pixels = Vec::with_capacity(sw * sh * if gray { 1 } else { 4 });
        for y in top..=bottom {
            let r = row(y).get(left * 4..(right + 1) * 4).unwrap_or(&[]);
            for p in r.as_chunks::<4>().0 {
                if gray {
                    // Rec. 601 luma.
                    let l = (u32::from(p[0]) * 299 + u32::from(p[1]) * 587 + u32::from(p[2]) * 114 + 500) / 1000;
                    pixels.push(l.min(255) as u8);
                } else {
                    pixels.extend_from_slice(&[p[2], p[1], p[0], 255]);
                }
            }
        }
        out.push(Strip { x: left, y: top, width: sw, height: sh, gray, pixels });
        y = bottom + 1;
    }
    out
}

/// The receipt the app shows: "spooled N sheet(s)" (the wording WordCraft translates).
fn receipt(pages: usize, copies: u32, duplex: Option<Duplex>) -> String {
    let sides = if matches!(duplex, Some(Duplex::LongEdge | Duplex::ShortEdge)) { 2 } else { 1 };
    format!("spooled {} sheet(s)", pages.div_ceil(sides).saturating_mul(copies.max(1) as usize))
}

// `print` is for hosts whose pages aren't a PDF (WordCraft hands over one).
#[cfg(target_os = "haiku")]
#[allow(unused_imports)]
pub use native::{print, print_pdf, printers, shift_down};

#[cfg(target_os = "haiku")]
mod native {
    use std::ffi::{CStr, CString, c_char, c_void};

    use super::{Duplex, Error, Options, PageSource, PdfPages, Printer, Store, failed, place, raster_scale, receipt, rotate_ccw, strips};

    /// Keep in step with `WcPrintOptions` in `native/print.cpp`.
    #[repr(C)]
    struct RawOptions {
        copies: i32,
        collate: i32,
        duplex: i32,
        grayscale: i32,
        pages: i32,
        color: i32,
    }

    /// Keep in step with `WcPrintJobInfo`.
    #[repr(C)]
    #[derive(Default)]
    struct RawJobInfo {
        paper: [f32; 4],
        printable: [f32; 4],
        xres: i32,
        yres: i32,
        first_page: i32,
        last_page: i32,
        copies: i32,
    }

    /// Keep in step with `WcPrintTile`.
    #[repr(C)]
    struct RawTile {
        pixels: *const u8,
        width: i32,
        height: i32,
        stride: i32,
        gray: i32,
        destination: [f32; 4],
    }

    #[repr(C)]
    struct RawJob {
        _private: [u8; 0],
    }

    unsafe extern "C" {
        fn wc_print_free(data: *mut c_void);
        fn wc_print_printers() -> *mut c_char;
        fn wc_print_settings(
            printer: *const c_char,
            mode: i32,
            settings: *const c_void,
            settings_len: usize,
            options: *const RawOptions,
            len: *mut usize,
            status: *mut i32,
        ) -> *mut c_void;
        fn wc_print_system_setup(settings: *const c_void, settings_len: usize, len: *mut usize, status: *mut i32) -> *mut c_void;
        fn wc_print_begin(settings: *const c_void, len: usize, title: *const c_char, info: *mut RawJobInfo, status: *mut i32) -> *mut RawJob;
        fn wc_print_progress(job: *mut RawJob, done: i32, total: i32);
        fn wc_print_cancelled(job: *mut RawJob) -> i32;
        fn wc_print_page(job: *mut RawJob, tiles: *const RawTile, count: i32, width: f32, height: f32, x: f32, y: f32, filter: i32) -> i32;
        fn wc_print_commit(job: *mut RawJob) -> i32;
        fn wc_print_cancel(job: *mut RawJob);
        fn wc_print_shift_down() -> i32;
    }

    // The Be API's status codes the native side reports.
    const CANCELED: i32 = i32::MIN + 12;
    const NO_INIT: i32 = i32::MIN + 13;
    const NO_MEMORY: i32 = i32::MIN;
    const ENTRY_NOT_FOUND: i32 = i32::MIN + 0x6000 + 3;
    const UNSUPPORTED: i32 = i32::MIN + 0x6000 + 14;

    fn c(s: &str) -> CString {
        CString::new(s.replace('\0', "")).unwrap_or_default()
    }

    /// The print server's printers, the default one marked.
    pub fn printers() -> Vec<Printer> {
        // SAFETY: no arguments; the result is a malloc'd C string or NULL.
        let raw = unsafe { wc_print_printers() };
        if raw.is_null() {
            return Vec::new();
        }
        // SAFETY: a NUL-terminated string from wc_print_printers, freed right after.
        let text = unsafe { CStr::from_ptr(raw) }.to_string_lossy().into_owned();
        // SAFETY: allocated by the native side with malloc.
        unsafe { wc_print_free(raw.cast()) };
        let mut list: Vec<Printer> = text
            .lines()
            .filter_map(|line| {
                let mut f = line.split('\t');
                let name = f.next().filter(|n| !n.is_empty())?.to_owned();
                Some(Printer {
                    name,
                    driver: f.next().unwrap_or_default().to_owned(),
                    transport: f.next().unwrap_or_default().to_owned(),
                    default: f.next() == Some("1"),
                    comments: f.next().unwrap_or_default().to_owned(),
                })
            })
            .collect();
        list.sort_by(|a, b| b.default.cmp(&a.default).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        list
    }

    /// Whether Shift is held now.
    pub fn shift_down() -> bool {
        // SAFETY: no arguments.
        unsafe { wc_print_shift_down() != 0 }
    }

    #[derive(Clone, Copy)]
    enum Mode {
        Defaults = 0,
        PageSetup = 1,
        JobSetup = 2,
        AsTheyAre = 3,
    }

    /// A printer's settings; without a printer, the system's print setup's.
    fn settings(printer: Option<&str>, mode: Mode, base: Option<&[u8]>, options: Option<&RawOptions>) -> Result<Vec<u8>, Error> {
        let (ptr, len) = base.map_or((std::ptr::null(), 0), |b| (b.as_ptr().cast::<c_void>(), b.len()));
        let opts = options.map_or(std::ptr::null(), |o| o as *const RawOptions);
        let (mut out_len, mut status) = (0usize, 0i32);
        let raw = match printer {
            Some(printer) => {
                let name = c(printer);
                // SAFETY: valid C string, buffer and options for the call's duration; the result
                // is malloc'd (`out_len` bytes) or NULL.
                unsafe { wc_print_settings(name.as_ptr(), mode as i32, ptr, len, opts, &mut out_len, &mut status) }
            }
            // SAFETY: a valid buffer for the call's duration; the result as above.
            None => unsafe { wc_print_system_setup(ptr, len, &mut out_len, &mut status) },
        };
        if raw.is_null() {
            return Err(match status {
                CANCELED => Error::Cancelled,
                UNSUPPORTED => Error::Unsupported,
                _ => failed(format!("the printer's settings can't be read ({})", status_text(status))),
            });
        }
        // SAFETY: `out_len` bytes at `raw`, freed right after the copy.
        let bytes = unsafe { std::slice::from_raw_parts(raw.cast::<u8>(), out_len) }.to_vec();
        // SAFETY: allocated by the native side with malloc.
        unsafe { wc_print_free(raw) };
        Ok(bytes)
    }

    fn status_text(status: i32) -> String {
        match status {
            ENTRY_NOT_FOUND => "the printer is gone".into(),
            NO_MEMORY => "out of memory".into(),
            s => format!("error {s:#x}"),
        }
    }

    /// A job between `wc_print_begin` and its commit or cancel.
    struct Job(*mut RawJob);

    impl Job {
        fn cancelled(&self) -> bool {
            // SAFETY: a live job.
            unsafe { wc_print_cancelled(self.0) != 0 }
        }

        fn progress(&self, done: usize, total: usize) {
            // SAFETY: a live job.
            unsafe { wc_print_progress(self.0, done.min(i32::MAX as usize) as i32, total.min(i32::MAX as usize) as i32) }
        }

        fn commit(mut self) -> Result<(), Error> {
            let job = std::mem::replace(&mut self.0, std::ptr::null_mut());
            // SAFETY: a live job, freed by the call.
            let status = unsafe { wc_print_commit(job) };
            if status == 0 { Ok(()) } else { Err(failed(format!("the print server didn't take the job ({})", status_text(status)))) }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: a live job that wasn't committed, freed by the call.
                unsafe { wc_print_cancel(self.0) };
            }
        }
    }

    /// Prints a PDF; see [`print`].
    pub fn print_pdf(printer: Option<&str>, pdf: Vec<u8>, options: &Options, store: &Store) -> Result<String, Error> {
        let mut pages = PdfPages::new(pdf)?;
        let mut options = options.clone();
        if options.title.is_empty() {
            options.title = pages.title().unwrap_or_default();
        }
        print(printer, &mut pages, &options, store)
    }

    /// Prints `source`'s pages on `printer` with its settings (the ones its setup dialogs left in
    /// `store`, else its driver's defaults) and `options`; without a printer, on the one the
    /// system's print setup picks. Returns the receipt the app shows. Blocks until the pages are
    /// spooled (and while setup dialogs are open): call it from a worker thread.
    pub fn print(printer: Option<&str>, source: &mut dyn PageSource, options: &Options, store: &Store) -> Result<String, Error> {
        let mut chosen: Vec<usize> = match &options.pages {
            Some(pages) => pages.iter().copied().filter(|&p| p < source.count()).collect(),
            None => (0..source.count()).collect(),
        };
        if chosen.is_empty() {
            return Err(failed("there are no pages to print"));
        }
        let raw = RawOptions {
            copies: options.copies.clamp(1, 999) as i32,
            collate: i32::from(options.collate),
            duplex: match options.duplex {
                None => -1,
                Some(Duplex::Off) => 0,
                Some(Duplex::LongEdge) => 1,
                Some(Duplex::ShortEdge) => 2,
            },
            grayscale: i32::from(options.grayscale),
            pages: chosen.len().min(i32::MAX as usize) as i32,
            color: i32::from(options.color && !options.grayscale),
        };
        // The system setup's settings are kept as those of a printer that can't exist ('/').
        let key = printer.unwrap_or("/system");
        let kept = store.load(key);
        let setup = options.setup || printer.is_none();
        let settings = if printer.is_none() {
            let chosen = settings(None, Mode::JobSetup, kept.as_deref(), None)?;
            store.save(key, &chosen);
            chosen
        } else if options.setup {
            let page = settings(printer, Mode::PageSetup, kept.as_deref(), Some(&raw))?;
            let job = settings(printer, Mode::JobSetup, Some(&page), None)?;
            store.save(key, &job);
            job
        } else {
            let applied = match kept.as_deref() {
                Some(kept) => settings(printer, Mode::AsTheyAre, Some(kept), Some(&raw)),
                None => settings(printer, Mode::Defaults, None, Some(&raw)),
            };
            match applied {
                // A driver without defaults (libprint's: PostScript, PCL, Gutenprint) is set up
                // once, as a Be application asks for Page Setup before its first print.
                Err(Error::Unsupported) => {
                    let page = settings(printer, Mode::PageSetup, None, Some(&raw))?;
                    store.save(key, &page);
                    // The page setup keeps page settings only: the job's go back on.
                    settings(printer, Mode::AsTheyAre, Some(&page), Some(&raw))?
                }
                other => other?,
            }
        };

        let title = c(if options.title.is_empty() { "Document" } else { &options.title });
        let mut info = RawJobInfo::default();
        let mut status = 0i32;
        // SAFETY: valid buffer, C string and out-pointers for the call's duration.
        let job = unsafe { wc_print_begin(settings.as_ptr().cast(), settings.len(), title.as_ptr(), &mut info, &mut status) };
        if job.is_null() {
            return Err(failed(if status == NO_INIT {
                "the print server has no default printer; choose one in the Printers preferences".to_owned()
            } else {
                format!("the print job can't start ({})", status_text(status))
            }));
        }
        let job = Job(job);
        if setup {
            // The job setup's page range counts the pages given.
            let first = usize::try_from(info.first_page.max(1) - 1).unwrap_or(0);
            let last = if info.last_page < 1 { chosen.len() } else { usize::try_from(info.last_page).unwrap_or(usize::MAX).min(chosen.len()) };
            chosen = chosen.get(first..last.max(first)).map(<[usize]>::to_vec).unwrap_or_default();
            if chosen.is_empty() {
                return Err(failed("the page range has no pages"));
            }
        }
        let landscape = options.landscape == Some(true);
        let device_dpi = info.xres.min(info.yres).max(1) as f32;
        let total = chosen.len();
        for (done, &page) in chosen.iter().enumerate() {
            job.progress(done, total);
            if job.cancelled() {
                return Err(Error::Cancelled);
            }
            let size = source.size(page);
            let at = place(size, info.paper, info.printable, landscape);
            // The page is rasterised as it reads, then turned.
            let (read_w, read_h) = if at.rotate { (at.height, at.width) } else { (at.width, at.height) };
            let scale = raster_scale(device_dpi, read_w, read_h);
            let (w, h) = ((read_w * scale).round().max(1.0) as usize, (read_h * scale).round().max(1.0) as usize);
            let mut image = source.render(page, scale * at.shrink, w, h)?;
            if at.rotate {
                image = rotate_ccw(&image);
            }
            let grayscale = options.grayscale;
            let strips = strips(&image, grayscale);
            drop(image);
            // A bitmap pixel is `1 / scale` points; a rectangle's right and bottom edges are the
            // last device dot's.
            let dot = 72.0 / device_dpi;
            let tiles: Vec<RawTile> = strips
                .iter()
                .map(|s| RawTile {
                    pixels: s.pixels.as_ptr(),
                    width: s.width as i32,
                    height: s.height as i32,
                    stride: (s.width * if s.gray { 1 } else { 4 }) as i32,
                    gray: i32::from(s.gray),
                    destination: [
                        s.x as f32 / scale,
                        s.y as f32 / scale,
                        (s.x + s.width) as f32 / scale - dot,
                        (s.y + s.height) as f32 / scale - dot,
                    ],
                })
                .collect();
            // Bitmaps drawn larger than they are (a 600 dpi printer) are smoothed.
            let filter = i32::from(scale * 72.0 < device_dpi - 1.0);
            // SAFETY: `tiles` and the strips' pixels they point to live through the call; the
            // native side copies them.
            let status = unsafe { wc_print_page(job.0, tiles.as_ptr(), tiles.len() as i32, at.width, at.height, at.x, at.y, filter) };
            if status != 0 {
                if job.cancelled() {
                    return Err(Error::Cancelled);
                }
                return Err(failed(format!("the page can't be spooled ({})", status_text(status))));
            }
        }
        job.progress(total, total);
        if job.cancelled() {
            return Err(Error::Cancelled);
        }
        job.commit()?;
        let copies = if printer.is_some() { options.copies } else { u32::try_from(info.copies.max(1)).unwrap_or(1) };
        Ok(receipt(total, copies, options.duplex))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A4: [f32; 4] = [0.0, 0.0, 595.0, 842.0];

    #[test]
    fn a_page_the_size_of_the_paper_prints_where_it_is() {
        let at = place((595.0, 842.0), A4, [18.0, 18.0, 577.0, 824.0], false);
        assert!(!at.rotate);
        assert_eq!((at.width, at.height, at.shrink), (595.0, 842.0, 1.0));
        // Measured from the printable rectangle: the paper's corner is 18 points up and left.
        assert_eq!((at.x, at.y), (-18.0, -18.0));
    }

    #[test]
    fn bigger_pages_shrink_and_smaller_ones_are_centred() {
        let letter = place((612.0, 792.0), A4, A4, false);
        assert!((letter.shrink - 595.0 / 612.0).abs() < 1e-4);
        assert!((letter.width - 595.0).abs() < 0.01);
        assert!(letter.y > 0.0);
        let a5 = place((420.0, 595.0), A4, A4, false);
        assert_eq!(a5.shrink, 1.0);
        assert_eq!((a5.x, a5.y), (87.5, 123.5));
    }

    #[test]
    fn landscape_pages_turn_on_portrait_paper() {
        let at = place((842.0, 595.0), A4, A4, false);
        assert!(at.rotate);
        assert_eq!((at.width, at.height, at.shrink), (595.0, 842.0, 1.0));
        // Landscape paper (the driver's page setup) takes it as it is.
        assert!(!place((842.0, 595.0), [0.0, 0.0, 842.0, 595.0], [0.0, 0.0, 842.0, 595.0], false).rotate);
        // Asking for landscape turns portrait pages too; square ones never turn.
        assert!(place((595.0, 842.0), A4, A4, true).rotate);
        assert!(!place((500.0, 500.0), A4, A4, true).rotate);
    }

    #[test]
    fn rasterising_stays_within_the_caps() {
        assert_eq!(raster_scale(300.0, 595.0, 842.0), 300.0 / 72.0);
        assert_eq!(raster_scale(1200.0, 595.0, 842.0), MAX_DPI / 72.0);
        assert_eq!(raster_scale(150.0, 595.0, 842.0), 150.0 / 72.0);
        // Nonsense resolutions get the default.
        assert_eq!(raster_scale(0.0, 595.0, 842.0), MAX_DPI / 72.0);
        // A poster is drawn coarser instead of taking gigabytes.
        let s = raster_scale(300.0, 2384.0, 3370.0);
        assert!(2384.0 * 3370.0 * s * s <= MAX_PIXELS * 1.001);
    }

    fn image(w: usize, h: usize, ink: &[(usize, usize, [u8; 3])]) -> Image {
        let mut rgba = vec![255u8; w * h * 4];
        for &(x, y, c) in ink {
            let i = (y * w + x) * 4;
            rgba[i..i + 3].copy_from_slice(&c);
        }
        Image { width: w, height: h, rgba }
    }

    #[test]
    fn rotation_puts_the_top_on_the_left() {
        let img = image(3, 2, &[(0, 0, [1, 2, 3]), (2, 1, [4, 5, 6])]);
        let r = rotate_ccw(&img);
        assert_eq!((r.width, r.height), (2, 3));
        // Top left goes to bottom left; bottom right to top right.
        assert_eq!(&r.rgba[(2 * 2) * 4..(2 * 2) * 4 + 3], &[1, 2, 3]);
        assert_eq!(&r.rgba[4..7], &[4, 5, 6]);
    }

    #[test]
    fn only_inked_rows_are_spooled() {
        assert!(strips(&image(10, 10, &[]), false).is_empty());
        let img = image(100, 100, &[(10, 5, [0, 0, 0]), (20, 7, [50, 50, 50]), (5, 60, [200, 0, 0])]);
        let s = strips(&img, false);
        assert_eq!(s.len(), 2);
        // Rows 5 and 7 are close enough to share a strip: columns 10..=20, rows 5..=7, grey.
        assert_eq!((s[0].x, s[0].y, s[0].width, s[0].height, s[0].gray), (10, 5, 11, 3, true));
        assert_eq!(s[0].pixels.len(), 33);
        assert_eq!(s[0].pixels[0], 0);
        // The red dot stays in colour, as B, G, R, A.
        assert_eq!((s[1].x, s[1].y, s[1].width, s[1].height, s[1].gray), (5, 60, 1, 1, false));
        assert_eq!(s[1].pixels, vec![0, 0, 200, 255]);
        // Grayscale prints it grey.
        let g = strips(&img, true);
        assert!(g[1].gray);
        assert_eq!(g[1].pixels, vec![60]);
    }

    #[test]
    fn receipts_count_sheets() {
        assert_eq!(receipt(3, 2, None), "spooled 6 sheet(s)");
        assert_eq!(receipt(3, 2, Some(Duplex::LongEdge)), "spooled 4 sheet(s)");
    }

    /// WordCraft's own print PDF renders: a sample document's first page has ink on white.
    #[test]
    fn a_wordcraft_pdf_rasterises() {
        let session = wordcraft_engine::Session::new(wordcraft_engine::sample::sample_document());
        let (pdf, _) = session.print_pdf(&serde_json::json!({})).unwrap();
        let mut pages = PdfPages::new(pdf).unwrap();
        assert!(pages.count() >= 1);
        let (w, h) = pages.size(0);
        let scale = 100.0 / 72.0;
        let (pw, ph) = ((w * scale).round() as usize, (h * scale).round() as usize);
        let img = pages.render(0, scale, pw, ph).unwrap();
        assert_eq!((img.width, img.height), (pw, ph));
        assert_eq!(&img.rgba[0..4], &[255, 255, 255, 255]);
        let s = strips(&img, false);
        assert!(!s.is_empty());
        assert!(s.iter().map(|s| s.width * s.height).sum::<usize>() < pw * ph);
    }
}
