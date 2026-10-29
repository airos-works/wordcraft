//! The air/OS side of [`crate::viewports`]: native windows are `host.cpp`'s `BDirectWindow`s,
//! frames are drawn with Vulkan (`gpu`, one presenter per window) or, without a hardware
//! adapter, by the software rasteriser (one canvas per window, the textures shared), and the
//! clipboard, cursors and links are the Be API's.

use std::collections::HashMap;
use std::time::Instant;

use crate::host;
use crate::raster::{Canvas, Rasterizer};
use crate::services;
use crate::viewports::{NativeCommand, Platform, WindowId, WindowInfo, WindowSpec};

/// What is drawn for one window.
#[derive(Default)]
struct Surface {
    canvas: Canvas,
    gpu: Option<crate::gpu::Surface>,
}

pub struct Haiku {
    raster: Rasterizer,
    /// The GPU renderer; `None` draws with `raster` (no hardware Vulkan adapter,
    /// `WORDCRAFT_RENDERER=cpu`, or the device was lost). The rasteriser keeps egui's textures
    /// either way, so it can take over between two frames.
    gpu: Option<crate::gpu::Gpu>,
    surfaces: HashMap<WindowId, Surface>,
}

/// Runs `f`; a panic inside it (a wgpu validation failure, a driver gone wrong) becomes an
/// error, so the windows fall back to the rasteriser instead of losing the open documents.
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

impl Haiku {
    pub fn new() -> Result<Self, String> {
        Ok(Self { raster: Rasterizer::new(), gpu: start_gpu()?, surfaces: HashMap::new() })
    }

    /// Back to the software rasteriser for good: the device failed (`why`). The next frames
    /// draw everything with it.
    fn drop_gpu(&mut self, why: &str) {
        for surface in self.surfaces.values_mut() {
            // The presenters let go of the windows before the device goes.
            surface.gpu = None;
            surface.canvas.invalidate();
        }
        if self.gpu.take().is_some() {
            log::error!("GPU: {why}; switching to the software rasteriser");
        }
    }

    fn surface(&mut self, window: WindowId) -> &mut Surface {
        let gpu = &self.gpu;
        let surface = self.surfaces.entry(window).or_default();
        if surface.gpu.is_none()
            && let Some(gpu) = gpu
        {
            surface.gpu = Some(gpu.surface(window));
        }
        surface
    }
}

impl Drop for Haiku {
    fn drop(&mut self) {
        // The presenters let go of their windows (and the frame buffer) before the device goes.
        self.surfaces.clear();
    }
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

impl Platform for Haiku {
    fn waker(&self) -> fn() {
        host::wake
    }

    fn open(&mut self, spec: &WindowSpec) -> Option<WindowId> {
        let window = host::open_window(spec);
        log::debug!("window {window:?} opened: {spec:?}");
        window
    }

    fn close(&mut self, window: WindowId) {
        // The presenter stops listening to the window's direct connection first.
        self.surfaces.remove(&window);
        host::close_window(window);
        log::debug!("window {window} closed");
    }

    fn command(&mut self, window: WindowId, command: NativeCommand) {
        host::window_command(window, &command);
    }

    fn info(&self, window: WindowId) -> WindowInfo {
        host::window_info(window)
    }

    fn set_cursor(&mut self, window: WindowId, cursor: egui::CursorIcon) {
        host::set_cursor(window, cursor_id(cursor));
    }

    fn output(&mut self, command: &egui::OutputCommand) {
        log::debug!("output {command:?}");
        match command {
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

    fn clipboard_text(&self) -> Option<String> {
        services::clipboard_text()
    }

    fn dark(&self) -> bool {
        host::dark()
    }

    fn max_texture_side(&self) -> usize {
        self.gpu.as_ref().map_or(8192, |g| g.max_texture_side().min(16384))
    }

    fn set_textures(&mut self, delta: &egui::TexturesDelta) {
        // The rasteriser keeps them too, to take over from a failed GPU.
        self.raster.set_textures(delta);
        if let Some(gpu) = &mut self.gpu
            && let Err(e) = guard("GPU textures", || {
                gpu.set_textures(delta);
                Ok(())
            })
        {
            self.drop_gpu(&e);
        }
    }

    fn free_textures(&mut self, ids: &[egui::TextureId]) {
        self.raster.free_textures(ids);
        if let Some(gpu) = &mut self.gpu {
            gpu.free_textures(ids);
        }
    }

    fn paint(&mut self, window: WindowId, size: [u32; 2], primitives: &[egui::ClippedPrimitive], ppp: f32, clear: egui::Color32, scale: f32) {
        if self.gpu.as_ref().is_some_and(|g| g.faulted()) {
            self.drop_gpu("the device was lost or failed");
        }
        let t0 = Instant::now();
        let mut failed = None;
        if self.gpu.is_some() {
            // The presenter shows GPU frames itself.
            self.surface(window);
            if let (Some(gpu), Some(Surface { gpu: Some(surface), .. })) = (&mut self.gpu, self.surfaces.get_mut(&window))
                && let Err(e) = guard("GPU frame", || gpu.render(surface, size, primitives, ppp, clear, scale))
            {
                failed = Some(e);
            }
        }
        if let Some(e) = failed {
            self.drop_gpu(&e);
            // This frame too.
        } else if self.gpu.is_some() {
            log::debug!("window {window} frame {}x{}: drawn in {:.1} ms", size[0], size[1], t0.elapsed().as_secs_f64() * 1e3);
            return;
        }
        let (w, h) = (size[0] as usize, size[1] as usize);
        let surface = self.surfaces.entry(window).or_default();
        let dirty = self.raster.render(&mut surface.canvas, [w, h], primitives, ppp, clear);
        let t1 = Instant::now();
        if let Some(dirty) = dirty {
            host::present(window, surface.canvas.pixels(), w, h, dirty, scale);
        }
        log::debug!(
            "window {window} frame {w}x{h}: raster {:.1} ms, present {:.1} ms, dirty {dirty:?}",
            (t1 - t0).as_secs_f64() * 1e3,
            t1.elapsed().as_secs_f64() * 1e3
        );
    }

    fn screenshot(&mut self, window: WindowId) -> egui::ColorImage {
        let Some(surface) = self.surfaces.get_mut(&window) else { return egui::ColorImage::default() };
        match &mut surface.gpu {
            Some(gpu) => gpu.image().unwrap_or_else(|e| {
                log::warn!("screenshot: {e}");
                egui::ColorImage::default()
            }),
            None => surface.canvas.image(),
        }
    }

    fn ime_caret(&mut self, window: WindowId, caret: Option<[f32; 4]>) {
        host::ime_caret(window, caret);
    }

    fn ime_stop(&mut self, window: WindowId) {
        host::ime_stop(window);
    }

    fn chores_due(&self) -> Option<Instant> {
        self.surfaces
            .values()
            .filter_map(|s| s.gpu.as_ref())
            .filter_map(|g| if g.wants_present() { Some(Instant::now()) } else { g.sync_due() })
            .min()
    }

    /// The last frame again where a window moved on the screen, and app_server's copy catching
    /// up with frames that went straight to the screen.
    fn chores(&mut self, scale: f32) {
        let now = Instant::now();
        let mut failed = None;
        for surface in self.surfaces.values_mut() {
            let Some(gpu) = &mut surface.gpu else { continue };
            let result = if gpu.wants_present() {
                guard("GPU present", || gpu.present_again(scale))
            } else if gpu.sync_due().is_some_and(|at| at <= now) {
                guard("GPU sync", || gpu.sync(scale))
            } else {
                continue;
            };
            if let Err(e) = result {
                failed = Some(e);
                break;
            }
        }
        if let Some(e) = failed {
            self.drop_gpu(&e);
        }
    }
}
