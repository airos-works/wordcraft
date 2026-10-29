use std::cell::RefCell;
use std::rc::Rc;

use egui::{Context, ViewportBuilder, ViewportCommand, ViewportId};

use super::*;

/// What the fake platform was asked to do.
#[derive(Default)]
struct Log {
    opened: Vec<(WindowId, WindowSpec)>,
    closed: Vec<WindowId>,
    commands: Vec<(WindowId, NativeCommand)>,
    painted: Vec<(WindowId, [u32; 2])>,
    ime_stops: Vec<WindowId>,
}

struct Fake {
    log: Rc<RefCell<Log>>,
    next: WindowId,
}

const ROOT_WINDOW: WindowId = 100;

impl Platform for Fake {
    fn waker(&self) -> fn() {
        || {}
    }

    fn open(&mut self, spec: &WindowSpec) -> Option<WindowId> {
        self.next += 1;
        self.log.borrow_mut().opened.push((self.next, spec.clone()));
        Some(self.next)
    }

    fn close(&mut self, window: WindowId) {
        self.log.borrow_mut().closed.push(window);
    }

    fn command(&mut self, window: WindowId, command: NativeCommand) {
        self.log.borrow_mut().commands.push((window, command));
    }

    fn info(&self, window: WindowId) -> WindowInfo {
        let x = (window - ROOT_WINDOW) as f32 * 50.0;
        WindowInfo {
            inner: [x, 30.0, x + 400.0, 330.0],
            outer: [x - 5.0, 5.0, x + 405.0, 335.0],
            monitor: [0.0, 0.0, 1920.0, 1080.0],
            active: window == ROOT_WINDOW,
            ..WindowInfo::default()
        }
    }

    fn set_cursor(&mut self, _window: WindowId, _cursor: egui::CursorIcon) {}

    fn output(&mut self, _command: &egui::OutputCommand) {}

    fn clipboard_text(&self) -> Option<String> {
        Some("pasted".into())
    }

    fn dark(&self) -> bool {
        false
    }

    fn max_texture_side(&self) -> usize {
        4096
    }

    fn set_textures(&mut self, _delta: &egui::TexturesDelta) {}

    fn free_textures(&mut self, _ids: &[egui::TextureId]) {}

    fn paint(&mut self, window: WindowId, size: [u32; 2], _primitives: &[egui::ClippedPrimitive], _ppp: f32, _clear: egui::Color32, _scale: f32) {
        self.log.borrow_mut().painted.push((window, size));
    }

    fn screenshot(&mut self, _window: WindowId) -> egui::ColorImage {
        egui::ColorImage::default()
    }

    fn ime_stop(&mut self, window: WindowId) {
        self.log.borrow_mut().ime_stops.push(window);
    }
}

/// A driver whose egui points are 2 device pixels, and a logical pixel one.
fn driver() -> (Context, Viewports<Fake>, Rc<RefCell<Log>>) {
    let ctx = Context::default();
    let log = Rc::new(RefCell::new(Log::default()));
    let v = Viewports::new(&ctx, Fake { log: log.clone(), next: ROOT_WINDOW }, ROOT_WINDOW, 2.0, 1.0);
    (ctx, v, log)
}

/// The root's pass, drawing `ui`.
fn frame(ctx: &Context, v: &Viewports<Fake>, ui: impl FnMut(&mut egui::Ui)) -> Finished {
    let Some(input) = v.root_input() else { return Finished::default() };
    let output = ctx.run_ui(input, ui);
    v.finish(ViewportId::ROOT, output)
}

fn child() -> ViewportId {
    ViewportId::from_hash_of("child")
}

/// What the root and the child pass saw: (viewport, its events, close requested).
type Seen = Rc<RefCell<Vec<(ViewportId, Vec<egui::Event>, bool)>>>;

fn show_child(seen: &Seen) -> impl FnMut(&mut egui::Ui) + '_ {
    move |ui| {
        let look = |ui: &egui::Ui| {
            let id = ui.ctx().viewport_id();
            ui.input(|i| (id, i.raw.events.clone(), i.viewport().close_requested()))
        };
        seen.borrow_mut().push(look(ui));
        ui.ctx().show_viewport_immediate(child(), ViewportBuilder::default().with_title("Second").with_inner_size([300.0, 200.0]), |ui, class| {
            assert!(class == egui::ViewportClass::Immediate);
            seen.borrow_mut().push(look(ui));
        });
    }
}

#[test]
fn an_immediate_viewport_gets_its_own_window_input_and_frame() {
    let (ctx, v, log) = driver();
    let seen: Seen = Rc::default();
    frame(&ctx, &v, show_child(&seen));
    let window = {
        let log = log.borrow();
        assert_eq!(log.opened.len(), 1, "one window for the child");
        let (window, spec) = &log.opened[0];
        assert_eq!(spec.title, "Second");
        // 300 × 200 points at 2 device pixels a point, 1 a logical pixel.
        assert_eq!(spec.size, [600.0, 400.0]);
        assert!(spec.decorations && spec.visible);
        assert_eq!(log.painted.iter().map(|p| p.0).collect::<Vec<_>>(), [*window, ROOT_WINDOW], "the child is drawn inside the root's pass");
        *window
    };
    assert_eq!(v.viewport_of(window), Some(child()));
    assert_eq!(v.window_of(child()), Some(window));

    // A click in the child's window reaches the child's pass alone, and runs the root's pass
    // (which draws it).
    assert!(v.input(window, WindowInput::Pointer { x: 20.0, y: 10.0, buttons: 1, modifiers: 0 }));
    assert_eq!(v.take_due(), [ViewportId::ROOT]);
    seen.borrow_mut().clear();
    frame(&ctx, &v, show_child(&seen));
    let seen = seen.borrow();
    assert!(seen[0].1.is_empty(), "the root saw none of it");
    assert_eq!(seen[1].0, child());
    // Logical pixels to points: 20 / 2.
    assert!(seen[1].1.contains(&egui::Event::PointerMoved(egui::pos2(10.0, 5.0))), "{:?}", seen[1].1);
    assert!(seen[1].1.iter().any(|e| matches!(e, egui::Event::PointerButton { pressed: true, .. })));
}

#[test]
fn the_close_button_asks_and_the_window_closes_when_the_app_drops_the_viewport() {
    let (ctx, v, log) = driver();
    let seen: Seen = Rc::default();
    frame(&ctx, &v, show_child(&seen));
    let window = log.borrow().opened[0].0;
    v.input(window, WindowInput::Close);
    seen.borrow_mut().clear();
    frame(&ctx, &v, show_child(&seen));
    assert_eq!(seen.borrow().iter().map(|s| s.2).collect::<Vec<_>>(), [false, true], "only the child's pass is asked to close");
    assert!(log.borrow().closed.is_empty(), "the window stays until the app stops showing it");
    // Asked once.
    seen.borrow_mut().clear();
    frame(&ctx, &v, show_child(&seen));
    assert!(!seen.borrow()[1].2);

    frame(&ctx, &v, |_| {});
    assert_eq!(log.borrow().closed, [window]);
    assert_eq!(v.viewport_of(window), None);
    assert!(!v.input(window, WindowInput::Focus(true)), "input for a closed window goes nowhere");
}

#[test]
fn viewport_commands_become_window_commands_in_logical_pixels() {
    let (ctx, v, log) = driver();
    let seen: Seen = Rc::default();
    frame(&ctx, &v, show_child(&seen));
    let window = log.borrow().opened[0].0;
    let finished = frame(&ctx, &v, |ui| {
        let ctx = ui.ctx().clone();
        ctx.send_viewport_cmd_to(child(), ViewportCommand::Title("Renamed".into()));
        ctx.send_viewport_cmd_to(child(), ViewportCommand::InnerSize(egui::vec2(100.0, 50.0)));
        ctx.send_viewport_cmd_to(child(), ViewportCommand::OuterPosition(egui::pos2(10.0, 20.0)));
        ctx.send_viewport_cmd_to(child(), ViewportCommand::Focus);
        ctx.send_viewport_cmd_to(child(), ViewportCommand::BeginResize(egui::ResizeDirection::SouthEast));
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
        show_child(&seen)(ui);
    });
    assert!(!finished.close_root);
    let commands = std::mem::take(&mut log.borrow_mut().commands);
    let child: Vec<NativeCommand> = commands.iter().filter(|(w, _)| *w == window).map(|(_, c)| c.clone()).collect();
    assert_eq!(
        child,
        [
            NativeCommand::Title("Renamed".into()),
            NativeCommand::ResizeTo([200.0, 100.0]),
            NativeCommand::MoveTo([20.0, 40.0]),
            NativeCommand::Activate,
            NativeCommand::BeginResize(edges::BOTTOM | edges::RIGHT),
        ]
    );
    assert!(commands.contains(&(ROOT_WINDOW, NativeCommand::Fullscreen(true))));

    let finished = frame(&ctx, &v, |ui| ui.ctx().send_viewport_cmd(ViewportCommand::Close));
    assert!(finished.close_root, "the host decides about the root");
}

#[test]
fn a_deferred_viewport_opens_a_window_and_repaints_on_its_own() {
    let (ctx, v, log) = driver();
    let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let id = ViewportId::from_hash_of("deferred");
    let show = |ui: &mut egui::Ui| {
        let runs = runs.clone();
        ui.ctx().show_viewport_deferred(id, ViewportBuilder::default().with_title("Deferred").with_inner_size([100.0, 100.0]), move |ui, class| {
            assert!(class == egui::ViewportClass::Deferred);
            assert_eq!(ui.ctx().viewport_id(), ViewportId::from_hash_of("deferred"));
            runs.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
    };
    v.take_due();
    frame(&ctx, &v, show);
    let window = log.borrow().opened[0].0;
    assert_eq!(v.viewport_of(window), Some(id));
    assert!(v.take_due().contains(&id), "its first pass is due");
    v.run_deferred(id);
    assert_eq!(runs.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(log.borrow().painted.iter().any(|p| p.0 == window));

    // Input in it runs its pass, not the root's.
    v.take_due();
    v.input(window, WindowInput::Focus(true));
    assert_eq!(v.take_due(), [id]);
}

#[test]
fn keys_and_paste_go_to_the_window_they_were_typed_in() {
    let (ctx, v, log) = driver();
    let seen: Seen = Rc::default();
    frame(&ctx, &v, show_child(&seen));
    let window = log.borrow().opened[0].0;
    // Command+V (B_COMMAND_KEY is 0x02).
    v.input(window, WindowInput::Key { pressed: true, code: 0x50, raw_char: 'v' as i32, modifiers: 0x02, text: "v" });
    seen.borrow_mut().clear();
    frame(&ctx, &v, show_child(&seen));
    assert!(seen.borrow()[1].1.contains(&egui::Event::Paste("pasted".into())), "{:?}", seen.borrow()[1].1);
}

#[test]
fn a_touchpad_gesture_scrolls_its_window_and_ends_a_pass_after_its_last_movement() {
    let (ctx, v, log) = driver();
    let seen: Seen = Rc::default();
    frame(&ctx, &v, show_child(&seen));
    let window = log.borrow().opened[0].0;
    let wheel = |dy: f32, phase: i32| WindowInput::Wheel { dx: 0.0, dy, modifiers: 0, gesture: Some(phase) };
    v.input(window, wheel(0.0, gesture::BEGAN));
    v.input(window, wheel(0.5, 2));
    v.input(window, wheel(0.0, gesture::ENDED));
    // A mouse wheel elsewhere stays whole lines.
    v.input(ROOT_WINDOW, WindowInput::Wheel { dx: 0.0, dy: 1.0, modifiers: 0, gesture: None });
    let phases = |events: &[egui::Event]| -> Vec<(egui::TouchPhase, egui::MouseWheelSource)> {
        events
            .iter()
            .filter_map(|e| match e {
                egui::Event::MouseWheel { phase, source, .. } => Some((*phase, *source)),
                _ => None,
            })
            .collect()
    };
    seen.borrow_mut().clear();
    frame(&ctx, &v, show_child(&seen));
    {
        let seen = seen.borrow();
        assert_eq!(phases(&seen[0].1), [(egui::TouchPhase::Move, egui::MouseWheelSource::Wheel)]);
        assert_eq!(
            phases(&seen[1].1),
            [(egui::TouchPhase::Start, egui::MouseWheelSource::Trackpad), (egui::TouchPhase::Move, egui::MouseWheelSource::Trackpad)]
        );
        let line = ctx.options(|o| o.input_options.line_scroll_speed);
        assert!(seen[1].1.iter().any(|e| matches!(e, egui::Event::MouseWheel { delta, .. } if *delta == egui::vec2(0.0, -0.5 * line))));
    }
    assert!(v.take_due().contains(&ViewportId::ROOT), "the gesture's end needs another pass");
    seen.borrow_mut().clear();
    frame(&ctx, &v, show_child(&seen));
    assert_eq!(phases(&seen.borrow()[1].1), [(egui::TouchPhase::End, egui::MouseWheelSource::Trackpad)]);
}

#[test]
fn an_input_method_composes_in_its_window_and_stops_when_nothing_there_takes_typing() {
    let (ctx, v, log) = driver();
    let seen: Seen = Rc::default();
    frame(&ctx, &v, show_child(&seen));
    let window = log.borrow().opened[0].0;
    // "nihongo" in kana, its first two characters (six bytes) the clause being converted.
    v.input(window, WindowInput::Preedit { text: "にほんご", selection: Some((0, 6)) });
    log.borrow_mut().ime_stops.clear();
    seen.borrow_mut().clear();
    frame(&ctx, &v, show_child(&seen));
    let preedit = egui::Event::Ime(egui::ImeEvent::Preedit { text: "にほんご".into(), active_range_chars: Some(0..2) });
    assert!(seen.borrow()[1].1.contains(&preedit), "{:?}", seen.borrow()[1].1);
    assert!(!seen.borrow()[0].1.contains(&preedit), "not the root's");
    // Nothing in the child takes typing (no IME output): its composition stops, and its
    // pre-edit goes in its next pass.
    assert!(log.borrow().ime_stops.contains(&window));
    seen.borrow_mut().clear();
    frame(&ctx, &v, show_child(&seen));
    let cleared = egui::Event::Ime(egui::ImeEvent::Preedit { text: String::new(), active_range_chars: None });
    assert!(seen.borrow()[1].1.contains(&cleared));

    v.input(window, WindowInput::Commit("日本語"));
    seen.borrow_mut().clear();
    frame(&ctx, &v, show_child(&seen));
    assert!(seen.borrow()[1].1.contains(&egui::Event::Ime(egui::ImeEvent::Commit("日本語".into()))));
}

#[test]
fn a_new_window_follows_its_builder() {
    let b = ViewportBuilder::default()
        .with_title("Panel")
        .with_position([10.0, 20.0])
        .with_inner_size([200.0, 100.0])
        .with_min_inner_size([50.0, 40.0])
        .with_decorations(false)
        .with_resizable(false)
        .with_window_level(egui::WindowLevel::AlwaysOnTop)
        .with_close_button(false);
    let s = spec(&b, 1.5);
    assert_eq!(s.title, "Panel");
    assert_eq!(s.position, Some([15.0, 30.0]));
    assert_eq!(s.size, [300.0, 150.0]);
    assert_eq!(s.min_size, Some([75.0, 60.0]));
    assert_eq!(s.max_size, None);
    assert!(!s.decorations && !s.resizable && s.floating && s.visible);
    assert_eq!(s.buttons, [false, true, true]);
}
