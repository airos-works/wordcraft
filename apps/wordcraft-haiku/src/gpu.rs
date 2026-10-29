//! Hardware rendering of the windows, as PhotoCraft's air/OS host does it. Haiku has no wgpu
//! surface: egui's triangles draw into a presenter's frame texture (`present.rs`), which gets
//! them onto the screen. One device and one egui renderer (the textures are the whole app's)
//! draw every window, each into a [`Surface`] of its own (its presenter). The software
//! rasteriser (`raster.rs`) stays for machines without a hardware Vulkan adapter,
//! `WORDCRAFT_RENDERER=cpu`, and a device that is lost while running.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use egui_wgpu::{RenderState, Renderer, RendererOptions, ScreenDescriptor, WgpuConfiguration, WgpuSetup};

use crate::host::Native;
use crate::present::{self, Presenter, Shown};

/// `WORDCRAFT_PRESENT`: how frames reach the window (`present::Mode`).
const PRESENT_VAR: &str = "WORDCRAFT_PRESENT";
const LOG: &str = "wordcraft_haiku";

pub struct Gpu {
    state: RenderState,
    mode: present::Mode,
    /// Set by the device's lost and uncaptured-error callbacks (any thread): the windows go back
    /// to the software rasteriser at the next frame.
    fault: Arc<AtomicBool>,
    /// Bumped by every texture upload, which can change any window's pixels.
    textures: u64,
}

/// One window's presenter.
pub struct Surface {
    presenter: Presenter,
    native: Native,
    /// What the frame on the window was drawn from ([`frame_key`], and the texture uploads so
    /// far): a frame drawn from the same is not drawn again. egui repaints more often than its
    /// output changes (a hover settling, a reply waiting on the next frame, another window's
    /// frame), and the software rasteriser skips those frames for nothing; here each would cost
    /// a submission and two waits for the GPU.
    last: Option<(u64, u64)>,
}

impl Drop for Surface {
    fn drop(&mut self) {
        self.presenter.detach(&self.native);
    }
}

impl Gpu {
    pub fn new() -> Result<Self, String> {
        let mut setup = WgpuSetup::without_display_handle();
        let WgpuSetup::CreateNew(ref mut create) = setup else { return Err("Expected a new GPU device".into()) };
        create.instance_descriptor.backends = wgpu::Backends::VULKAN;
        // Software Vulkan (lavapipe) draws slower than the rasteriser does.
        create.native_adapter_selector = Some(Arc::new(|adapters, _surface| {
            adapters
                .iter()
                .find(|a| matches!(a.get_info().device_type, wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu))
                .cloned()
                .ok_or_else(|| "No hardware Vulkan adapter; using software rendering".to_string())
        }));
        let instance = pollster::block_on(setup.new_instance());
        let config = WgpuConfiguration { wgpu_setup: setup, ..Default::default() };
        let mut state = pollster::block_on(RenderState::create(&config, &instance, None, RendererOptions::default())).map_err(|e| e.to_string())?;
        // Frames are drawn in the screen's byte order: they go to the frame buffer, or into a
        // B_RGB32 bitmap, as they are.
        state.target_format = present::FORMAT;
        state.renderer = Arc::new(egui::mutex::RwLock::new(Renderer::new(&state.device, present::FORMAT, RendererOptions::default())));
        let fault = Arc::new(AtomicBool::new(false));
        let lost = fault.clone();
        state.device.set_device_lost_callback(move |reason, message| {
            log::error!(target: LOG, "GPU device lost ({reason:?}): {message}");
            lost.store(true, Ordering::Relaxed);
        });
        // wgpu's default handler panics.
        let failed = fault.clone();
        state.device.on_uncaptured_error(Arc::new(move |e| {
            log::error!(target: LOG, "GPU error: {e}");
            failed.store(true, Ordering::Relaxed);
        }));
        log::info!(target: LOG, "air/OS GPU: {:?}", state.adapter.get_info());
        Ok(Self { state, mode: present::Mode::from_env(PRESENT_VAR), fault, textures: 0 })
    }

    /// A presenter for native window `window`.
    pub fn surface(&self, window: i32) -> Surface {
        let native = Native(window);
        let presenter = Presenter::new(&self.state.device, &self.state.queue, self.mode, &native);
        log::info!(target: LOG, "air/OS present (window {window}): {}", presenter.mode().name());
        Surface { presenter, native, last: None }
    }

    /// egui's texture uploads, before the frames that use them.
    pub fn set_textures(&mut self, delta: &egui::TexturesDelta) {
        if delta.set.is_empty() {
            return;
        }
        self.textures += 1;
        let state = &self.state;
        let mut renderer = state.renderer.write();
        for (id, images) in &delta.set {
            for image in images {
                renderer.update_texture(&state.device, &state.queue, *id, image);
            }
        }
    }

    /// Textures egui freed, once every window's frame that used them is drawn.
    pub fn free_textures(&mut self, ids: &[egui::TextureId]) {
        let mut renderer = self.state.renderer.write();
        for id in ids {
            renderer.free_texture(id);
        }
    }

    pub fn max_texture_side(&self) -> usize {
        self.state.device.limits().max_texture_dimension_2d as usize
    }

    /// Whether the device was lost or failed: the window should go back to the rasteriser.
    pub fn faulted(&self) -> bool {
        self.fault.load(Ordering::Relaxed)
    }

    /// Draws a window's frame and shows it (`scale` device pixels per logical pixel). The
    /// textures it uses are uploaded ([`Gpu::set_textures`]).
    pub fn render(
        &mut self,
        surface: &mut Surface,
        size: [u32; 2],
        primitives: &[egui::ClippedPrimitive],
        ppp: f32,
        clear: egui::Color32,
        scale: f32,
    ) -> Result<Shown, String> {
        let key = frame_key(size, primitives, ppp, clear).map(|k| (k, self.textures));
        if key.is_some() && key == surface.last && !surface.presenter.wants_present() {
            return Ok(Shown { direct: false, pixels: 0, rects: 0, wait: std::time::Duration::ZERO });
        }
        surface.last = None;
        let state = &self.state;
        let view = surface.presenter.target(size)?;
        let start = Instant::now();
        let screen = ScreenDescriptor { size_in_pixels: size, pixels_per_point: ppp };
        let mut encoder = state.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("WordCraft native frame") });
        let mut renderer = state.renderer.write();
        let mut commands = renderer.update_buffers(&state.device, &state.queue, &mut encoder, primitives, &screen);
        {
            let rgba = clear.to_array().map(|c| f64::from(c) / 255.0);
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("WordCraft native UI"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color { r: rgba[0], g: rgba[1], b: rgba[2], a: rgba[3] }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            renderer.render(&mut pass, primitives, &screen);
        }
        commands.push(encoder.finish());
        drop(renderer);
        let encoded = start.elapsed();
        let shown = surface.presenter.present(commands, &surface.native, scale)?;
        surface.last = key;
        log::debug!(target: LOG,
            "GPU frame {}x{}: encode {:.2} ms, present {:.2} ms ({}, {} px in {} rects)",
            size[0],
            size[1],
            encoded.as_secs_f64() * 1e3,
            shown.wait.as_secs_f64() * 1e3,
            if shown.direct { "direct" } else { "readback" },
            shown.pixels,
            shown.rects
        );
        Ok(shown)
    }
}

impl Surface {
    /// The last frame again, where the window's place on the screen changed.
    pub fn present_again(&mut self, scale: f32) -> Result<Shown, String> {
        self.presenter.present(Vec::new(), &self.native, scale)
    }

    pub fn wants_present(&self) -> bool {
        self.presenter.wants_present()
    }

    pub fn sync_due(&self) -> Option<Instant> {
        self.presenter.sync_due()
    }

    /// Brings app_server's copy of the window up to date with what went straight to the screen.
    pub fn sync(&mut self, scale: f32) -> Result<Shown, String> {
        self.presenter.sync(&self.native, scale)
    }

    pub fn image(&mut self) -> Result<egui::ColorImage, String> {
        self.presenter.image()
    }
}

/// A small, fast hash (FxHash's mix, as `raster.rs`) of everything that decides a frame's pixels,
/// or `None` for a frame that must be drawn (a paint callback draws what the hash can't see).
/// Texture uploads are checked by the caller.
fn frame_key(size: [u32; 2], primitives: &[egui::ClippedPrimitive], ppp: f32, clear: egui::Color32) -> Option<u64> {
    let mut k: u64 = 0;
    let mut add = |v: u64| k = (k.rotate_left(5) ^ v).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    add(u64::from(size[0]) << 32 | u64::from(size[1]));
    add(u64::from(ppp.to_bits()));
    add(u64::from(u32::from_le_bytes(clear.to_array())));
    for p in primitives {
        let r = p.clip_rect;
        add(u64::from(r.min.x.to_bits()) << 32 | u64::from(r.min.y.to_bits()));
        add(u64::from(r.max.x.to_bits()) << 32 | u64::from(r.max.y.to_bits()));
        let egui::epaint::Primitive::Mesh(mesh) = &p.primitive else { return None };
        add(match mesh.texture_id {
            egui::TextureId::Managed(n) => n,
            egui::TextureId::User(n) => n ^ (1 << 63),
        });
        add(mesh.indices.len() as u64);
        for &i in &mesh.indices {
            add(u64::from(i));
        }
        for v in &mesh.vertices {
            add(u64::from(v.pos.x.to_bits()) << 32 | u64::from(v.pos.y.to_bits()));
            add(u64::from(v.uv.x.to_bits()) << 32 | u64::from(v.uv.y.to_bits()));
            add(u64::from(u32::from_le_bytes(v.color.to_array())));
        }
    }
    Some(k)
}
