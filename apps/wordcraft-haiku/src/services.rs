//! The app's system services on air/OS: Open and Save panels (`BFilePanel`), the clipboard
//! (`BClipboard`: text, HTML, RTF, pictures) and making WordCraft the preferred application for
//! Word documents (`BMimeType`).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};

use wordcraft_engine::cmd::paste::MAX_PASTE_BYTES;
use wordcraft_ui_egui::paste_picture::{ClipboardPicture, MAX_FILE_BYTES};
use wordcraft_ui_egui::{DefaultApp, FileDialogAnswer, FileDialogRequest, Services};

use crate::host;

/// The application's signature (also in `WordCraft.rdef`).
pub const SIGNATURE: &str = "application/x-vnd.storytold-WordCraft";

/// The Word document types: WordCraft becomes their preferred application on first start where
/// none is set ([`claim_document_types`]), and for all of them through File › Options › Make
/// WordCraft the default.
const DOCUMENT_TYPES: &[&str] = &[
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.template",
    "application/vnd.ms-word.document.macroEnabled.12",
    "application/msword",
    "application/rtf",
    "text/rtf",
    "application/vnd.oasis.opendocument.text",
];

/// Makes WordCraft the preferred application of the Word document types that have none, so a
/// double-click in Tracker opens them (Tracker otherwise asks which program to use).
pub fn claim_document_types() {
    let claimed = host::claim_types(SIGNATURE, DOCUMENT_TYPES);
    if claimed > 0 {
        log::info!("preferred application for {claimed} document types that had none");
    }
}

/// The file panels the app asked for and are still open, by id.
#[derive(Default)]
pub struct Dialogs {
    next: Cell<i32>,
    pending: RefCell<HashMap<i32, Sender<FileDialogAnswer>>>,
    saves: RefCell<std::collections::HashSet<i32>>,
    /// Paths picked in Save panels, for the shell to type once written.
    pub saved: RefCell<Vec<std::path::PathBuf>>,
}

impl Dialogs {
    /// The panel `id` closed: with a path, or cancelled.
    pub fn answer(&self, id: i32, picked: bool, path: String) {
        let save = self.saves.borrow_mut().remove(&id);
        if let Some(tx) = self.pending.borrow_mut().remove(&id) {
            if save && picked && !path.is_empty() {
                self.saved.borrow_mut().push(path.clone().into());
            }
            let paths = if picked { path.lines().filter(|p| !p.is_empty()).map(String::from).collect() } else { Vec::new() };
            let _ = tx.send(Ok(paths));
        }
    }

    fn show(&self, req: FileDialogRequest) -> Receiver<FileDialogAnswer> {
        let (tx, rx) = channel();
        let id = self.next.get().wrapping_add(1);
        self.next.set(id);
        // One panel at a time (as the app asks): an older one still open counts as cancelled.
        for (_, old) in self.pending.borrow_mut().drain() {
            let _ = old.send(Ok(Vec::new()));
        }
        self.pending.borrow_mut().insert(id, tx);
        let open = |purpose: &str, panel| {
            let filters = wordcraft_ui_egui::open_filters(purpose);
            let exts: &[&str] = filters.first().map_or(&["*"], |(_, e)| e);
            let title = match purpose {
                "picture" => "Insert Picture",
                "font" => "Add Fonts",
                "recipients" => "Select Recipients",
                "sources" => "Import Sources",
                _ => "Open",
            };
            host::file_panel(id, panel, "", exts, &format!("WordCraft: {title}"), None);
        };
        match req {
            FileDialogRequest::Open { purpose } => open(&purpose, host::Panel::Open),
            FileDialogRequest::OpenMany { purpose } => open(&purpose, host::Panel::OpenMany),
            FileDialogRequest::Save { name, dir } => {
                self.saves.borrow_mut().insert(id);
                host::file_panel(id, host::Panel::Save, &name, &[], "WordCraft: Save As", dir.as_deref());
            }
        }
        rx
    }
}

fn clip_text(mime: &str) -> Option<String> {
    let bytes = host::clipboard_get(mime)?;
    if bytes.len() > MAX_PASTE_BYTES {
        return None;
    }
    let s = String::from_utf8_lossy(&bytes).trim_end_matches('\0').to_string();
    (!s.is_empty()).then_some(s)
}

/// The clipboard's plain text.
pub fn clipboard_text() -> Option<String> {
    clip_text("text/plain")
}

fn clipboard_rtf() -> Option<String> {
    ["text/rtf", "application/rtf"].iter().find_map(|m| clip_text(m)).filter(|s| s.trim_start().starts_with("{\\rtf"))
}

/// Picture types other programs put on the clipboard encoded, best first.
const PICTURE_TYPES: [&str; 5] = ["image/png", "image/jpeg", "image/gif", "image/webp", "image/bmp"];

fn clipboard_picture() -> Option<ClipboardPicture> {
    if let Some((width, height, rgba)) = host::clipboard_bitmap() {
        return Some(ClipboardPicture::Pixels { width, height, rgba });
    }
    PICTURE_TYPES.iter().find_map(|m| host::clipboard_get(m)).map(|bytes| {
        if bytes.len() as u64 > MAX_FILE_BYTES {
            ClipboardPicture::Unreadable("the picture is too large".into())
        } else {
            ClipboardPicture::Encoded(bytes)
        }
    })
}

/// The services the app gets on air/OS.
pub fn services(dialogs: Rc<Dialogs>) -> Services {
    Services {
        file_dialog: Some(Box::new(move |req| dialogs.show(req))),
        clipboard_text: Some(Box::new(clipboard_text)),
        clipboard_html: Some(Box::new(|| clip_text("text/html"))),
        // An equation's MathML isn't offered separately: other Haiku programs take the HTML or text.
        set_clipboard_html: Some(Box::new(|html, text, _mathml| host::clipboard_set(text, Some(html)))),
        clipboard_rtf: Some(Box::new(clipboard_rtf)),
        clipboard_formats: Some(Box::new(host::clipboard_formats)),
        clipboard_picture: Some(Box::new(clipboard_picture)),
        // Personal templates live in `Templates` in the settings folder (#2852), as on the desktop.
        templates_dir: wordcraft_control_key::settings_dir().map(|d| d.join("Templates")),
        make_default_app: Some(Box::new(|| host::make_default(SIGNATURE, DOCUMENT_TYPES).map(|()| DefaultApp::MadeDefault))),
        // Haiku has no animation setting, and every frame here is drawn by the processor: the
        // caret's fade and glide (a dozen frames a second while idle) are off unless
        // `WORDCRAFT_ANIMATIONS=1`; File › Options › Advanced can still turn them on.
        animations_enabled: Some(Box::new(|| Some(std::env::var_os("WORDCRAFT_ANIMATIONS").is_some_and(|v| v != "0")))),
        ..Default::default()
    }
}
