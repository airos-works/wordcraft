# WordCraft on air/OS (and Haiku)

Status: first working build, 2026-10-17. WordCraft runs as a native air/OS application: the
whole interface (ribbon, backstage, dialogs, the page canvas) and everything below it are the
same code as on the other desktops; only the host around the interface is new.

## How it fits together

| Desktop app (`apps/wordcraft`) | air/OS app (`apps/wordcraft-haiku`) |
| --- | --- |
| eframe + winit windows, one per egui viewport | `BApplication` + a `BDirectWindow` and `BView` per egui viewport (`native/host.cpp`, `src/viewports.rs`, `src/platform.rs`) |
| wgpu renders egui's meshes | wgpu/Vulkan renders them offscreen, changed tiles shown in a `BBitmap` (`src/gpu.rs`, `src/present.rs`); without a hardware GPU, a software rasteriser (`src/raster.rs`) |
| rfd file dialogs | `BFilePanel` Open (one file, or several for Add Fonts) and Save panels |
| arboard / wl-clipboard | `BClipboard` (`text/plain`, `text/html`, MathML, RTF, pictures, archived `BBitmap`s) |
| winit cursors | `BCursor` system cursors |
| `xdg-mime` default app | `BMimeType::SetPreferredApp` for the Word document types |
| OS locale | `BLocaleRoster` (preferred languages, formatting conventions) |
| Finder / Explorer open | `B_REFS_RECEIVED` (Tracker double-click, Open With, drops), one running copy (`B_SINGLE_LAUNCH`) |
| CUPS / Windows spooler (File › Print) | the print kit: the print server's printers, pages drawn into a `BPrintJob` (`src/print.rs`, `native/print.cpp`) |

eframe and winit don't run on Haiku, so `WordApp` (the interface, `crates/ui-egui`) is
driven directly: the host turns Be API messages into egui input, runs `WordApp::logic` and
`WordApp::ui` exactly as eframe does, and turns egui's output back into window, cursor,
clipboard and URL calls. Nothing below the interface knows it is on Haiku apart from a few
paths: the settings folder (`~/config/settings/WordCraft`), the font folders
(`/boot/system/data/fonts`, `~/config/non-packaged/data/fonts`…), the spelling dictionaries
(`…/data/hunspell`), the CJK interface font, and the recovery copies' owner locks (`flock`,
as Rust's `File::try_lock` is unsupported on Haiku: copies started from different files, such
as a test build beside the package, run at the same time, and a running copy's recovery copy
mustn't look like a crash's).

### One running copy

WordCraft is `B_SINGLE_LAUNCH` (`WordCraft.rdef`): one copy runs from each executable file.
Documents opened in Tracker while it runs (double-click, Open With, drops on its icon) arrive in
the running copy as `B_REFS_RECEIVED`, and a launch from Terminal (`WordCraft letter.docx`)
hands its arguments to it (`B_ARGV_RECEIVED`, relative paths resolved against the launch's
folder; options such as `--sample` and `--control` are ignored) and ends before it touches the
log file, the preferences or the recovery copies (`BApplication` reports `B_ALREADY_RUNNING`).
The documents open as the desktop app opens the ones macOS hands it
(`WordApp::open_file`): like File › Open, beside the open ones, each in a window of its own
(see Windows below), which comes to the front. A launch without documents only brings the
active window up, un-minimized (also the Deskbar's, `B_SILENT_RELAUNCH`, which `BApplication`
alone answers by activating the team, leaving a minimized window down). The copy's own
arguments, which `BApplication` posts to itself before `B_READY_TO_RUN` without a reply address
(so `IsSourceRemote()` calls them remote), are left to the Rust side, which opened them
already. Quitting (Command+Q, the Deskbar's
Quit) asks about every open document with unsaved changes, as the desktop app does for macOS's
quit event, then quits. A test copy that must run beside the installed one runs from a file of
its own.

Threads: `be_app` runs on the main thread, each window on its looper thread, and the interface
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

### Updates and community links

air/OS installs and updates WordCraft as a package (pkgman, SoftwareUpdater): the Help tab has no
Check for Updates button, and the command (from the command search or the control channel)
says SoftwareUpdater installs newer versions instead of offering upstream's downloads
(`updates_ui.rs`, `ribbon/review.rs`). The start-up check stays off. The community (Discord)
buttons stay: they open the web browser on the same app's community, and upstream has no switch
for the host to hide them (File › Options' title bar button setting is the user's).

### Scrolling

A mouse wheel scrolls in whole steps, as before. An air/OS touchpad's scroll gesture
(`B_MOUSE_WHEEL_CHANGED` with `be:device_subtype` 2, `be:wheel_precise_delta_*` and
`be:wheel_phase`, air/OS since 2026-10-27) reaches the interface as a trackpad's: its unrounded
steps become points (a step is egui's line, 40 points), with the gesture's start and end, so
the page follows the fingers one to one, coasts after a flick and egui's lists treat it as a
trackpad. The end of a gesture that arrives with its last movement is held for the next frame
(egui drops the deltas of a gesture that ended). air/OS's touchpad has no pinch gesture, so
zooming is Command+wheel or the zoom slider.

### Input methods

The canvas view is `B_INPUT_METHOD_AWARE`: an input method (Japanese, Chinese, Korean…) composes
in the editor itself. Its `B_INPUT_METHOD_EVENT`s become egui's IME events: `CHANGED` is the
pre-edit (its `be:selection` the active clause), `CHANGED` with `be:confirmed` the commit, and
`STOPPED` without one drops the pre-edit. A `LOCATION_REQUEST` is answered with the caret
(egui's `IMEOutput::cursor_rect`, in screen coordinates) for each character, so the candidate
window opens by it. When the interface interrupts the composition (the caret moves away, the
editor loses the keyboard) the host sends the input method `B_INPUT_METHOD_STOPPED` and ignores
what it sends afterwards until it starts again.

### Equations on the clipboard

A copy puts its text and HTML (equations in it as MathML) on the clipboard. A copy of one
equation alone also puts its MathML in `application/mathml-presentation+xml` and
`application/mathml+xml`, the MIME types W3C MathML 3 (§6.2.4) registers for exchanging it, as
the desktop app adds the platform's MathML formats (Windows' "MathML", macOS'
`public.mathml.presentation`).

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
- `tools/haiku/input-probe.cpp` (built on air/OS: `g++ -o input-probe input-probe.cpp -lbe`)
  sends a running copy the input a test machine can't make: an input method's composition
  (`ime`, `ime-cancel`), a pointer position, a touchpad gesture or wheel turn; it also lists the
  clipboard's types and opens files with a given copy as Tracker does (`open`).
- `RUST_LOG=WordCraft=debug` logs keys, clipboard output and each frame's timings (interface,
  tessellation, rasteriser, present) to `~/config/settings/WordCraft/logs/wordcraft.log`.

## Known gaps

- A window's maximized state isn't reported (Haiku has zoom, not maximize).

## Windows: one `BWindow` per egui viewport (2026-10-29, #6)

The desktop app shows each open document in a window of its own, and View › New Window opens
another window on the same document, with its own caret, zoom and view (upstream's
`crates/ui-egui/src/windows.rs`, driven by craft-libs' `craft_window::Viewports`). Each of
those windows is an egui *immediate* viewport; the window in the root viewport is the first
one, and when it closes while others stay open, the root viewport takes over another window
(it moves and resizes to that window's place, as on the desktop). The host used to embed
viewports in the one window; it now gives every viewport a native window, as eframe does:

- New and Open (also from Tracker) open the document in a new window, cascaded from the last
  one; View › New Window opens a second window on the document (`Name:1`, `Name:2` in the
  titles). View › Switch Windows brings one forward.
- A window's close button closes that window only (File › Close, asking about unsaved changes
  when it is the document's last window). The last window's close button quits, asking first;
  Command+Q (or the Deskbar's Quit) asks about every unsaved document, each in its window.
- The geometry saved for the next start is the active window's, as the desktop app's.
- Full Screen (F11) puts that window, without its border, over the monitor it is on
  (`BPrivate::get_display_frame` on air/OS, the screen elsewhere) and back.

### Design (`src/viewports.rs`, for the other Craft hosts too)

`src/viewports.rs` is the multi-viewport driver and knows nothing about Haiku: the other Craft
hosts can take it unchanged (DeckCraft's presenter view and slide show, PhotoCraft's floating
panels, LightCraft's second window are all egui viewports). It needs a `Platform`:

| `Platform` | air/OS (`src/platform.rs`) |
| --- | --- |
| `open`, `close`, `command`, `info` (logical pixels) | `wc_host_open_window` etc. in `host.cpp`: a registry of `WcWindow`s by id |
| `paint(window, size, triangles…)`, `screenshot` | Vulkan: one device and egui renderer (shared textures), a presenter (`present.rs`) per window; CPU: one `Rasterizer` (shared textures), a `Canvas` per window |
| `set_textures`, `free_textures` | both renderers; frees wait until the outermost pass has drawn every window |
| `set_cursor`, `output` (clipboard, links), `clipboard_text`, `dark` | the Be API |
| `chores_due`, `chores` | each presenter's direct-mode repaints and app_server catch-up |

How a frame goes:

1. `host.cpp` tags every input event with its window's id (`WcEvent.window`); the shell hands
   window events to `Viewports::input`, which turns them into that viewport's egui events
   (pointer, buttons, wheel and touchpad gestures, keys and the clipboard shortcuts,
   modifiers, focus, drops, input method pre-edits and commits) and puts a close button's
   press in that viewport's `ViewportInfo::events`. Each window's canvas has its own input
   method transaction; a pass's `PlatformOutput::ime` moves its window's caret for the
   candidate window (`Platform::ime_caret`) or stops that window's composition
   (`Platform::ime_stop`), never another window's. Application events
   (Tracker's refs, file panel answers, quit) stay the shell's.
2. `Viewports::take_due` says whose pass is due: the root's, or a deferred viewport's. An
   immediate viewport is drawn inside its parent's pass, so its input and its repaint
   requests (egui's repaint callback names the viewport) run the root's pass.
3. The root's pass is the shell's: `Viewports::root_input` gives its `RawInput` (its window's
   size and every viewport's `ViewportInfo`: inner/outer rectangles, monitor size, focus,
   minimised, full screen), the shell runs the app's logic and UI and hands the output to
   `Viewports::finish`.
4. While the app draws, each `show_viewport_immediate` lands in the renderer the driver
   installed (`Context::set_immediate_viewport_renderer`): it opens the viewport's window on
   first use (from its `ViewportBuilder`: title, position, size limits, decorations,
   resizable, visible, maximised, full screen, `WindowLevel::AlwaysOnTop`, title bar
   buttons), runs the viewport's pass with that window's input and finishes it.
5. `finish` (every pass): cursor and output commands, texture uploads, then for every
   viewport in the output: deferred ones get a window (and a pass of their own), builder
   changes (`ViewportBuilder::patch`) and `ViewportCommand`s become native commands (title,
   move, resize, size limits, minimise, zoom, full screen, decorations, resizable, level,
   focus, visible, `StartDrag`, `BeginResize`, buttons, cut/copy/paste, screenshots), then the
   pass is drawn into its window. After the outermost pass, windows whose viewport egui
   dropped close, and freed textures go. A `ViewportCommand::Close` to the root comes back to
   the host (`Finished::close_root`); to another viewport it is a close request in its input.

Native side: `host.cpp` keeps its windows in a map by id; every call locks the window it
names. Only the interface thread opens and closes windows, so a window it looked up can't go
away under it. Each window has its own direct connection (`DirectConnected` reports to the
presenter of that window, by id), cursor, full screen state and drag state. `StartDrag` and
`BeginResize` (undecorated windows that draw their own title bar and edges) move the window,
or its edges, with the pointer while the button stays down. `wc_host_quit` closes every
window first (a window made hidden is still locked by the interface thread).

Tests: `cargo test -p wordcraft-haiku` runs the driver against a fake platform
(`src/viewports/tests.rs`: a window per immediate viewport, input reaching only its
viewport's pass, close requests and windows closing when the app drops the viewport,
commands in logical pixels, deferred viewports repainting on their own, paste per window).
On air/OS (x399, `WORDCRAFT_RENDERER` auto and cpu, `WORDCRAFT_PRESENT` readback and
direct with `PRESENT_VERIFY=1`): View › New Window, typing into the second window and seeing
it in the first, closing a second window, closing the root's window (the root takes over
the other window's place), a second document in its own window (from Tracker's open path,
`input-probe open`, and from a second launch in Terminal handing its arguments over), Full
Screen and back in a second window, a touchpad gesture and an input method composition in a
third window (the location reply at that window's caret, the commit in its document only),
and quitting with unsaved changes from a close button and from `B_QUIT_REQUESTED`. Scripted input for such tests has to go to the canvas view's own
`BMessenger` (`get Messenger of View "canvas" of Window N`): `BView` ignores input messages
that still carry specifiers.

Not exercised by WordCraft (driver-tested only): deferred viewports, undecorated and
floating windows, `StartDrag`/`BeginResize`, hidden windows. Haiku has no "is zoomed" state,
so `Maximized(false)` does nothing and `ViewportInfo::maximized` is always false; a window
opens activated (`with_active(false)` is ignored); `with_monitor` and `SetMonitor` are not
implemented (place the window with `OuterPosition` from the monitor frames instead).

## Printing (2026-10-29, #1)

File › Print lists the print server's printers (the folders in `~/config/settings/printers`, as
the Printers preferences show them; the default printer first) and prints on the one chosen with
upstream's settings: copies and collation, two-sided printing and grayscale where the driver has
them, and the pages, orientation and Scale to paper size that went into the print-ready PDF.

- Each page of the PDF is rasterised by hayro (pure Rust, as PdfCraft draws its pages) at the
  printer's resolution, at most 300 dpi: a 600 dpi printer's driver scales the bitmap up (with
  bilinear filtering). The page goes on the printer's paper at its real size, centred, shrunk only
  when it is bigger (Letter on A4), and turned a quarter when a landscape page (or any page, with
  Orientation › Landscape) meets portrait paper.
- Only the strips of rows with ink are spooled, grey ones with one byte a pixel, as `BBitmap`s
  drawn by an off-screen view into the job (`DrawView`, `SpoolPage`, `CommitJob`): a page of
  black text takes about 60 KB of spool file and the sample document's two pages, with coloured
  headings and tables, 7.9 MB, where whole pages would take 35 MB each. 61 pages of text spool in
  about 6 s on the x399.
- A small window shows the pages being spooled and cancels the job (its spool file is removed).
  The print server then runs the driver; its jobs can be cancelled in the Printers preferences.
- A printer's settings come from its driver. Preview has defaults; libprint's drivers (PostScript,
  PCL, Gutenprint, LIPS) have none, so the first print on such a printer opens its Paper setup
  (paper, orientation, resolution), as a Be application asks for Page Setup before printing.
  Shift+Print opens the driver's page setup and job setup every time (`WORDCRAFT_PRINT_SETUP=1`
  always): paper source, quality, colour and the other driver options. Their answers are kept per
  printer in `~/config/settings/WordCraft/printers/<printer>.settings`; the job setup's page
  range and copies then apply as well.
- BPrintJob's own ConfigPage/ConfigJob go to the print server's default printer, whose setup
  window has a printer menu of its own, so the chosen printer's driver is reached through
  `print_addon_server`, as the print server reaches it. BPrintJob spools into the default
  printer's folder and the print server moves the job to the settings' `current_printer`; where
  no default printer is set, the printer printed to becomes it.
- Grayscale sets the driver to monochrome; otherwise a job asks for colour (libprint's
  PostScript and PCL drivers start in monochrome), unless the printer's job setup has been
  answered (Shift+Print), whose choice then stays (`craft:color_chosen` in the kept settings).
  The system's print setup (no printer listed) can't be given a colour: the print server's
  setup window starts the driver's job setup from its own settings for the printer, not the
  application's, so its Color choice decides there. The upstream printer model has a name and the default flag only: paper sizes, colour
  and two-sided support aren't listed (the driver's dialogs show them).
- Tested on the x399 with a PostScript printer whose JetDirect transport writes to a local
  `nc -l -p 9100`: mixed portrait and landscape pages, Letter on A4, copies (collated by the
  driver), colour, grayscale, Orientation › Landscape, cancelling a 61-page job from the progress
  window and cancelling the setup dialogs. The Preview driver takes the jobs but its window draws
  nothing on air/OS hrev60207, for any program's job (a plain `BPrintJob` of `FillRect` and
  `DrawString` too).

`src/print.rs` and `native/print.cpp` are self-contained so that the other Craft hosts can take
them with the `wc_print_` prefix renamed: `printers()`, `print_pdf(printer, pdf, options,
store)`, or `print()` with a `PageSource` for pages that aren't a PDF; without a printer, the
system's print setup (the print server's window with its printer menu) chooses one.

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
