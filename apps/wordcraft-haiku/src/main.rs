//! WordCraft for air/OS (and Haiku).
//!
//! The same interface as the desktop app (`wordcraft-ui-egui`), hosted natively: a
//! `BApplication` and `BWindow` from the Be API (`native/host.cpp`) take the input and show the
//! frames, which egui's output becomes through a software rasteriser (`raster`), with the
//! system's file panels, clipboard, cursors, file types and locale. eframe, winit and wgpu don't
//! run on Haiku; nothing below the interface changes.
//!
//! Usage: `WordCraft [--sample] [--control PORT] [FILE…]`, as the desktop app.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
// Elsewhere only the platform-neutral parts build (and are tested).
#![cfg_attr(not(target_os = "haiku"), allow(dead_code))]

mod keys;
mod raster;

#[cfg(target_os = "haiku")]
#[path = "../../wordcraft/src/control_server.rs"]
mod control_server;
#[cfg(target_os = "haiku")]
mod host;
#[cfg(target_os = "haiku")]
#[path = "../../wordcraft/src/logging.rs"]
mod logging;
#[cfg(target_os = "haiku")]
#[path = "../../wordcraft/src/region.rs"]
mod region;
#[cfg(target_os = "haiku")]
mod services;
#[cfg(target_os = "haiku")]
mod shell;

#[cfg(not(target_os = "haiku"))]
fn main() {
    let _ = std::io::Write::write_all(&mut std::io::stderr(), b"WordCraft for air/OS runs on Haiku; elsewhere run `wordcraft`.\n");
    std::process::exit(1);
}

#[cfg(target_os = "haiku")]
use haiku::*;

#[cfg(target_os = "haiku")]
fn main() {
    haiku::main()
}

#[cfg(target_os = "haiku")]
mod haiku {
    use std::rc::Rc;

    use wordcraft_engine::Session;
    use wordcraft_ui_egui::window_geometry::WindowGeometry;
    use wordcraft_ui_egui::{UiState, WordApp};

    use crate::{control_server, host, logging, region, services, shell};

    const USAGE: &str = "\
WordCraft — the WordCraft word processor for air/OS

USAGE:
  WordCraft [--sample] [--control PORT] [FILE…]

OPTIONS:
  --sample            open the sample document
  --control PORT      start the localhost control server on PORT, for agents and scripts
                      (or set WORDCRAFT_CONTROL_PORT); see docs/control-protocol.md
  --version           print the version
  -h, --help          print this help
";

    /// The default window: content size in points, and the smallest.
    const DEFAULT_SIZE: [f32; 2] = [1280.0, 820.0];
    const MIN_SIZE: [f32; 2] = [760.0, 480.0];

    /// `ui.json` in the settings folder (`~/config/settings/WordCraft`).
    fn prefs_path() -> Option<std::path::PathBuf> {
        wordcraft_control_key::settings_dir().map(|b| b.join("ui.json"))
    }

    fn log_dir() -> Option<std::path::PathBuf> {
        Some(prefs_path()?.parent()?.join("logs"))
    }

    /// Runs without preferences (`WORDCRAFT_NO_PREFS`) neither read nor write them.
    fn prefs_enabled() -> bool {
        std::env::var_os("WORDCRAFT_NO_PREFS").is_none()
    }

    fn load_prefs(app: &mut WordApp) {
        if !prefs_enabled() {
            return;
        }
        if let Some(p) = prefs_path()
            && let Ok(bytes) = std::fs::read(&p)
            && let Some(ui) = UiState::from_json(&bytes)
        {
            app.apply_prefs(ui);
        }
    }

    fn load_user_dictionary(app: &mut WordApp) {
        if !prefs_enabled() {
            return;
        }
        if let Some(p) = wordcraft_control_key::settings_dir().map(|b| b.join(Session::USER_DICT_FILE)) {
            wordcraft_proof::load_user_dictionary(&p);
            app.session.user_dict_path = Some(p);
        }
    }

    fn load_user_fonts() {
        if !prefs_enabled() {
            return;
        }
        let Some(dir) = wordcraft_control_key::settings_dir().map(|d| d.join("fonts")) else { return };
        let (loaded, skipped) = wordcraft_fonts::FontDb::global().load_user_fonts(&dir);
        if !loaded.is_empty() || !skipped.is_empty() {
            log::info!("user fonts: {} loaded, {} skipped from {}", loaded.len(), skipped.len(), dir.display());
        }
    }

    fn saved_window() -> Option<WindowGeometry> {
        if !prefs_enabled() {
            return None;
        }
        let bytes = std::fs::read(prefs_path()?).ok()?;
        UiState::from_json(&bytes)?.window?.sanitized()
    }

    pub fn save_prefs(app: &WordApp) {
        if !prefs_enabled() {
            return;
        }
        let Some(p) = prefs_path() else { return };
        if let Some(dir) = p.parent()
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            log::warn!("preferences: cannot create {}: {e}", dir.display());
            return;
        }
        match serde_json::to_vec_pretty(&app.prefs()) {
            Ok(bytes) => {
                // Through a temporary file, so a failed write keeps the old preferences.
                let tmp = p.with_file_name(format!(".ui.json.{}.tmp", std::process::id()));
                let written = std::fs::write(&tmp, &bytes).and_then(|()| std::fs::rename(&tmp, &p));
                if let Err(e) = written {
                    let _ = std::fs::remove_file(&tmp);
                    log::warn!("preferences: cannot save {}: {e}", p.display());
                }
            }
            Err(e) => log::warn!("preferences: cannot serialize: {e}"),
        }
    }

    fn build_sha() -> &'static str {
        option_env!("WORDCRAFT_BUILD_SHA").map(|s| s.get(..8).unwrap_or(s)).unwrap_or("dev")
    }

    struct Args {
        sample: bool,
        control: Option<u16>,
        files: Vec<String>,
    }

    fn args() -> Args {
        let mut a = Args { sample: false, control: std::env::var("WORDCRAFT_CONTROL_PORT").ok().and_then(|p| p.parse().ok()), files: Vec::new() };
        let mut it = std::env::args_os().skip(1).map(|s| s.to_string_lossy().into_owned()).peekable();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--sample" => a.sample = true,
                "--control" => match it.next().and_then(|p| p.parse::<std::num::NonZeroU16>().ok()) {
                    Some(p) => a.control = Some(p.get()),
                    None => {
                        let _ = std::io::Write::write_all(&mut std::io::stderr(), b"WordCraft: --control needs a port number from 1 to 65535\n");
                        std::process::exit(2);
                    }
                },
                "--version" => {
                    let _ = std::io::Write::write_fmt(
                        &mut std::io::stdout(),
                        format_args!("WordCraft {} ({}) for air/OS\n", env!("CARGO_PKG_VERSION"), build_sha()),
                    );
                    std::process::exit(0);
                }
                "--help" | "-h" => {
                    let _ = std::io::Write::write_all(&mut std::io::stdout(), USAGE.as_bytes());
                    std::process::exit(0);
                }
                _ => a.files.push(arg),
            }
        }
        a
    }

    /// The interface thread: builds the app and runs the window's event loop.
    fn interface(args: Args, restored: Option<WindowGeometry>, scale: f32) {
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(true);
        let region = region::system();
        let doc = if args.sample { wordcraft_engine::sample::sample_document() } else { wordcraft_engine::blank_document(&region) };
        let mut session = Session::new(doc);
        session.system_region = region;
        let dialogs = Rc::new(services::Dialogs::default());
        let mut app = WordApp::new(session, services::services(dialogs.clone()));
        load_prefs(&mut app);
        load_user_dictionary(&mut app);
        if !args.sample && app.session.prefs.region.is_some() {
            let doc = app.session.blank_document();
            app.session.set_document(doc);
        }
        if prefs_enabled()
            && let Some(dir) = wordcraft_control_key::settings_dir()
        {
            let c = ctx.clone();
            if let Err(e) = app.session.enable_recovery(dir.join("recovery"), std::sync::Arc::new(move || c.request_repaint())) {
                log::warn!("local recovery unavailable: {e}");
            }
        }
        app.ui.window = restored;
        app.integrated_titlebar = false;
        let mut key_file = None;
        if let Some(port) = args.control
            && let Some((rx, file)) = control_server::start(port, ctx.clone(), wordcraft_control_key::settings_dir().as_deref())
        {
            app = app.with_control(rx);
            key_file = file;
        }
        if args.files.is_empty() && !args.sample && args.control.is_none() {
            app.start_screen();
        }
        for f in &args.files {
            if let Err(e) = app.run("file.open", serde_json::json!({"path": f})) {
                log::warn!("{f}: {e}");
            }
        }
        let mut shell = shell::Shell::new(app, ctx, dialogs, restored, scale);
        shell.key_file = key_file;
        shell.run();
        shell.finish();
    }

    pub fn main() {
        // Before any thread starts: the locale variables come from Haiku's Locale preferences.
        host::export_locale();
        let logger = logging::install();
        logging::install_panic_hook();
        let args = args();
        if let Some(logger) = logger {
            match log_dir().filter(|_| prefs_enabled()) {
                Some(dir) => match logger.attach_dir(&dir) {
                    Ok(path) => log::info!("WordCraft {} ({}) for air/OS, log file {}", env!("CARGO_PKG_VERSION"), build_sha(), path.display()),
                    Err(e) => log::warn!("no log file: {e}"),
                },
                None => logger.no_file(),
            }
        }
        if let Err(e) = host::init(services::SIGNATURE) {
            log::error!("{e}");
            std::process::exit(1);
        }
        if prefs_enabled() {
            services::claim_document_types();
        }
        load_user_fonts();
        let scale = host::ui_scale();
        let restored = saved_window();
        let px = |v: f32| v * scale;
        let size = restored.map_or(DEFAULT_SIZE, |g| [g.width, g.height]);
        host::open_window(&host::WindowSpec {
            title: "WordCraft".into(),
            position: restored.and_then(|g| g.position).map(|p| p.map(px)),
            size: size.map(px),
            min_size: MIN_SIZE.map(px),
            zoomed: restored.is_some_and(|g| g.maximized),
        });
        let ui = std::thread::Builder::new().name("WordCraft interface".into()).stack_size(16 << 20).spawn(move || {
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| interface(args, restored, scale)));
            if ran.is_err() {
                log::error!("the interface stopped after an internal error");
                host::alert("WordCraft", "WordCraft ran into an internal error and has to close. Your document's recovery copy is kept in ~/config/settings/WordCraft/recovery.");
            }
            host::quit();
        });
        match ui {
            Ok(handle) => {
                host::run();
                let _ = handle.join();
            }
            Err(e) => log::error!("couldn't start the interface: {e}"),
        }
    }
}
