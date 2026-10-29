# WordCraft on air/OS (and Haiku)

Status: first working build, 2026-10-17. WordCraft runs as a native air/OS application: the
whole interface (ribbon, backstage, dialogs, the page canvas) and everything below it are the
same code as on the other desktops; only the host around the interface is new.

## How it fits together

| Desktop app (`apps/wordcraft`) | air/OS app (`apps/wordcraft-haiku`) |
| --- | --- |
| eframe + winit window | `BApplication` + `BWindow` + `BView` (`native/host.cpp`) |
| wgpu renders egui's meshes | wgpu/Vulkan renders them offscreen, changed tiles shown in a `BBitmap` (`src/gpu.rs`, `src/present.rs`); without a hardware GPU, a software rasteriser (`src/raster.rs`) |
| rfd file dialogs | `BFilePanel` Open (one file, or several for Add Fonts) and Save panels |
| arboard / wl-clipboard | `BClipboard` (`text/plain`, `text/html`, RTF, pictures, archived `BBitmap`s) |
| winit cursors | `BCursor` system cursors |
| `xdg-mime` default app | `BMimeType::SetPreferredApp` for the Word document types |
| OS locale | `BLocaleRoster` (preferred languages, formatting conventions) |
| Finder / Explorer open | `B_REFS_RECEIVED` (Tracker double-click, Open With, drops) |

eframe and winit don't run on Haiku, so `WordApp` (the interface, `crates/ui-egui`) is
driven directly: the host turns Be API messages into egui input, runs `WordApp::logic` and
`WordApp::ui` exactly as eframe does, and turns egui's output back into window, cursor,
clipboard and URL calls. Nothing below the interface knows it is on Haiku apart from a few
paths: the settings folder (`~/config/settings/WordCraft`), the font folders
(`/boot/system/data/fonts`, `~/config/non-packaged/data/fonts`…), the spelling dictionaries
(`…/data/hunspell`), the CJK interface font, and the recovery copies' owner locks (`flock`,
as Rust's `File::try_lock` is unsupported on Haiku: each double-clicked document starts its
own copy of WordCraft, and a running copy's recovery copy mustn't look like a crash's).

Threads: `be_app` runs on the main thread, the window on its looper thread, and the interface
on a thread of its own, which waits on the host's event queue between frames. Only
`src/host.rs` (the FFI calls) and `src/scanout.rs` (the raw Vulkan frame buffer import) contain
`unsafe` code; the crate denies it everywhere else.

### Drawing

With a hardware Vulkan adapter (`WORDCRAFT_RENDERER=auto`, the default; `vulkan` requires one,
`cpu` never uses it) egui-wgpu draws the frame into a BGRA texture (`src/gpu.rs`), and the
presenter shared with PhotoCraft's and LightCraft's hosts (`src/present.rs`, `damage.rs`,
`scanout.rs`) compares it with what app_server shows, 64 px tile by tile, and copies back only
the tiles that changed. A frame drawn from the same triangles, size and textures as the one on
the screen is not drawn again. `WORDCRAFT_PRESENT=readback|full|direct` picks how frames reach
the window; `direct` (straight into the screen's frame buffer with air/OS's
`VkImportScanoutMemoryHAIKU`) is experimental, since RADV on the Framework 13 loses the device a
few seconds after the import. A lost device or a GPU error switches the window to the software
rasteriser below without losing the open documents. wgpu's Vulkan backend is enabled for Haiku
by the vendored `wgpu`, `wgpu-core` and `wgpu-core-deps-windows-linux-android` 30.0.1
(`vendor/README.md`).

Without one, egui's meshes are premultiplied sRGB triangles. `raster.rs` draws them on the processor:
axis-aligned rectangles (panels, glyphs, pictures, the rendered pages) and flat triangles take
fast paths; the frame is split into bands of 32 rows drawn in parallel (rayon), and a band whose
shapes, clips and textures are unchanged since the last frame is skipped, so a blinking caret
or a keystroke redraws a few bands, not the window. Only the rectangle that changed is copied
into the `BBitmap` and redrawn.

On an air/OS desktop drawn at a higher density (200 % on 4K monitors), the frame is rendered
in device pixels and drawn with `DrawBitmap(bitmap, device rect, logical rect)`, so app_server
copies it one to one and text stays sharp. The density comes from air/OS's display layout
(`BPrivate::get_display_layout`, looked up at run time, so plain Haiku gets 1); the interface
scale follows the system font size (12 pt = 100 %). `WORDCRAFT_DEVICE_SCALE` overrides the
density.

Animations (the caret's fade and glide) are off by default: Haiku has no system setting for
them, and without a hardware GPU every frame is drawn by the processor. File › Options ›
Advanced › Animation, or `WORDCRAFT_ANIMATIONS=1`, turns them back on.

### Keys

Haiku's Command key (left Alt by default) and Control both act as Word's Ctrl, so Command+B,
Command+S, Command+C/V work as Haiku users expect and Ctrl+B as Word users expect. Option (the
Windows key) is Word's Alt (key tips). Command+Q quits the Haiku way (with the Save Changes
prompt); `BWindow`'s own Command+X/C/V/A/W shortcuts are removed so the interface gets them.
Letters are identified by the keymap (`raw_char`), so shortcuts follow the user's layout.

## Building

The Rust code is cross-compiled on Linux with current stable Rust (the workspace needs
Rust 1.95; HaikuPorts has 1.94), building `std` for the tier-3 target `x86_64-unknown-haiku`
from `rust-src`:

```sh
# once: rustup with rust-src, and a sysroot of an air/OS machine's development files
export RUSTUP_HOME=/mnt/HaikuWork/toolchains/rust/rustup CARGO_HOME=/mnt/HaikuWork/toolchains/rust/cargo
curl -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal -c rust-src
# sysroot: /boot/system/develop/{lib,headers} copied (dereferenced) into
# $HAIKU_SYSROOT/boot/system/develop, plus libstdc++/libgcc_s from gcc_syslibs

bash tools/haiku/build.sh          # target/x86_64-unknown-haiku/release/WordCraft
bash tools/haiku/package.sh        # artifacts/haiku/wordcraft-<version>-x86_64.hpkg
```

`tools/haiku/env.sh` sets the toolchain up (the Haiku cross gcc for linking and for
`native/host.cpp`, `RUSTC_BOOTSTRAP=1` for `-Zbuild-std`). The package step needs a Haiku
machine for `rc`, `xres`, `mimeset` and `package`: the test VM, or the workstation with
`WC_PKG_HOST=ws`.

air/OS CI builds the package on every push to `haiku` (`.github/workflows/airos.yml`, which
calls `jmgasper/airos-ci`'s app pipeline: `build_wordcraft` in `apps/build-app-packages.sh`
uses the shared nightly Rust and the SDK's host `rc`, `xres`, `mimeset` and `package`, so no
Haiku machine is involved). The package goes to the air/OS package pool, the x86_64 images
include it, and the fork's rolling `latest` release carries it. The package's post-install
script (`resources/wordcraft-post-install.sh`) registers the installed application with the MIME
database and makes it the preferred application of the Word document types that have none, so
Tracker opens `.docx` files with it before it has ever run.

The platform-neutral parts (the rasteriser, the key tables) are tested anywhere:
`cargo test -p wordcraft-haiku`. Elsewhere the binary only says it needs Haiku.

## Testing

- `tools/haiku/vm/run-vm.sh` boots a throwaway Haiku VM (VNC 127.0.0.1:5909, SSH 2229) as an
  overlay on another project's disk; `tools/haiku/vm/haiku.sh` runs commands in it,
  `tools/haiku/vm/vm.py` takes screenshots and sends clicks, keys and wheel turns, and
  `tools/haiku/vm/ctl.sh` talks to the control channel of a copy started with `--control 7981`
  (e.g. `ui.screenshot`).
- `RUST_LOG=WordCraft=debug` logs keys, clipboard output and each frame's timings (interface,
  tessellation, rasteriser, present) to `~/config/settings/WordCraft/logs/wordcraft.log`.

## Known gaps

- No printing yet (Haiku's print kit is not wired up); export to PDF works.
- No input methods (`B_INPUT_METHOD_EVENT`) yet, so CJK input methods don't reach the canvas.
- Fullscreen and the window's maximized state aren't reported (Haiku has zoom, not maximize).
- View › New Window opens the new document window inside the main one (egui's embedded
  viewports): the host has one `BWindow`.
- Copying an equation puts its HTML and text on the clipboard, not its MathML.

## Upstream 0.12.0 (2026-10-29)

Merged storytold/wordcraft at `aadb077e` (0.12.0, egui 0.37 from storytold's egui fork). The
host follows the API changes: screenshots complete egui's `ScreenshotCallback`, wheel events
say they come from a wheel, the window logic uses `RawWindowState` like the desktop app's,
Add Fonts gets a multiple-selection Open panel, Save as Word Template starts the Save panel in
the templates folder (`~/config/settings/WordCraft/Templates`, which also lists personal
templates in File › New), and the File › Recover page follows the app theme as upstream's does.
Package `0.12.0~airos1-1`.

## Vulkan rendering (2026-10-29)

Package `0.12.0~airos1-2` draws the window with Vulkan (Drawing). Framework 13 (Radeon 760M,
175 %, the sample document, 30 requests each through the control channel, against the
rasteriser in the same build): WordCraft CPU 1.85 s -> 0.96 s, app_server unchanged
(0.12 s); hover 18.6 -> 17.1 ms and caret moves 18.9 -> 17.6 ms per request.

## Native application icon (2026-10-26)

The air/OS icon is an original folded writing page and blue fountain pen, licensed under
MIT OR Apache-2.0. Its 64-unit vector drawing follows the
[Haiku icon guidelines](https://www.haiku-os.org/development/icon-guidelines):
vertical edges stay upright in perspective, the main light comes from the top
left, the right faces are darker, a floor shadow extends to the right, and the
silhouette has a two-unit black outline (one pixel at 32px). Broad details are
intended to remain legible at 16px and 32px. No fonts or third-party artwork are used.

Rebuild both the native HVIF and its SVG preview from this checkout:

```sh
python3 tools/haiku/make-icon.py
```

`apps/wordcraft-haiku/resources/WordCraft.hvif` is the packaged icon;
`apps/wordcraft-haiku/resources/WordCraft.svg` exports the same geometry for inspection.
The existing `.rdef` imports the HVIF, and `tools/haiku/package.sh` embeds it in
the executable's resources. Tracker and Deskbar therefore use the vector icon.
The upstream icons for other platforms remain separate. Native previews can be
rendered on Haiku with `BIconUtils::GetVectorIcon`; review at 16, 32, 64 and 128px
on light and dark backgrounds before changing the drawing.

Package version `0.7.0~airos1-7` includes this icon update.
