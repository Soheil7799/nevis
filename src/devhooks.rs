//! Developer hooks, driven by environment variables (unused in normal runs):
//!
//! - `NEVIS_BENCH=1`: print how long it took until the file was on screen, then quit.
//!   With `NEVIS_T0=$(date +%s%N)` the time before `main()` (exec, linking) is included.
//! - `NEVIS_SNAPSHOT=out.png`: save a picture of the window once it is drawn, then quit.
//! - `NEVIS_ACTIONS=win.reading,win.switch-to:/path/a.md`: actions to run first (for the
//!   picture); `:` gives an action its string argument.
//! - `NEVIS_TYPE=text`: then type it at the cursor, as a user would (replacing the selection;
//!   in a field, a final newline presses Enter). In a text view, `{Tab}`, `{S-Tab}`, `{Enter}`
//!   and `{End}` press those keys, through the view's own key handlers.
//! - `NEVIS_PASTE_IMAGE=file.png`: then paste that image at the cursor (instead of what the
//!   clipboard holds, which test runs share with the desktop).
//! - `NEVIS_WAIT=ms`: how long to wait before typing and the picture (default 500).
//! - `NEVIS_SETTLE=ms`: how long to wait after typing, before the picture (default 300).
//! - `NEVIS_SESSION=1`: keep and restore the session, which these runs otherwise leave alone
//!   (with `XDG_STATE_HOME=/tmp/x`, away from the real one).
//! - `NEVIS_APP_ID=dev.shirvani.NevisTest`: a separate instance, leaving the real service alone.
//! - `NEVIS_TRACE=1 nevis-app --gapplication-service`: print checkpoints, and when each window's
//!   text is on screen.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gtk::{gdk, glib, prelude::*};

static START: OnceLock<Instant> = OnceLock::new();

pub fn mark_start() {
    START.get_or_init(Instant::now);
}

pub fn active() -> bool {
    static ACTIVE: OnceLock<bool> = OnceLock::new();
    *ACTIVE.get_or_init(|| {
        std::env::var_os("NEVIS_BENCH").is_some() || std::env::var_os("NEVIS_SNAPSHOT").is_some()
    })
}

/// With `NEVIS_BENCH` (or `NEVIS_TRACE`) set, prints how far into the run `label` was reached.
pub fn lap(label: &str) {
    static TRACE: OnceLock<bool> = OnceLock::new();
    if active() || *TRACE.get_or_init(|| std::env::var_os("NEVIS_TRACE").is_some()) {
        let ms = START.get().map_or(0.0, |s| s.elapsed().as_secs_f64() * 1000.0);
        eprintln!("  {ms:8.1} ms  {label}");
    }
}

/// Called once the window's content is in place; reports after the next painted frame.
pub fn content_ready(window: &gtk::ApplicationWindow) {
    // NEVIS_TRACE=1 on a long-running instance: print when each window's text is on screen
    // (ns since the epoch, to compare with the launching shell's `date +%s%N`).
    if std::env::var_os("NEVIS_TRACE").is_some() {
        on_next_paint(window, || {
            lap("text painted");
            eprintln!("nevis: text on screen at {}", epoch_ns());
        });
    }
    // Once per run: switching files fires this again, and the run must only end once.
    static REPORTED: AtomicBool = AtomicBool::new(false);
    if !active() || REPORTED.swap(true, Ordering::Relaxed) {
        return;
    }
    // Stamp the frame as it is painted: the report itself runs later, after other work
    // waiting on the same frame (highlighting, colouring code).
    let painted = Rc::new(Cell::new(None));
    let stamp = painted.clone();
    on_next_paint(window, move || stamp.set(Some((since_start_ms(), epoch_ns()))));
    let target = window.clone();
    crate::editor::after_next_frame(window, move || report(target, painted.get()));
}

/// Runs `f` inside the next frame's after-paint, once.
pub fn on_next_paint(window: &gtk::ApplicationWindow, f: impl FnOnce() + 'static) {
    let Some(clock) = window.frame_clock() else { return };
    let pending = RefCell::new(Some(f));
    let handler = Rc::new(RefCell::new(None));
    let handler_in = handler.clone();
    let id = clock.connect_after_paint(move |clock| {
        if let Some(f) = pending.borrow_mut().take() {
            f();
            if let Some(id) = handler_in.borrow_mut().take() {
                clock.disconnect(id);
            }
        }
    });
    *handler.borrow_mut() = Some(id);
    window.queue_draw();
}

fn since_start_ms() -> f64 {
    START.get().map_or(0.0, |s| s.elapsed().as_secs_f64() * 1000.0)
}

fn epoch_ns() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

/// `painted`: when the frame was painted, as (ms since main, ns since the epoch).
fn report(window: gtk::ApplicationWindow, painted: Option<(f64, u128)>) {
    if std::env::var_os("NEVIS_BENCH").is_some() {
        let (in_main, at) = painted.unwrap_or_else(|| (since_start_ms(), epoch_ns()));
        let since_launch = std::env::var("NEVIS_T0")
            .ok()
            .and_then(|t0| t0.trim().parse::<u128>().ok())
            .and_then(|t0| at.checked_sub(t0))
            .map(|ns| ns as f64 / 1e6);
        match since_launch {
            Some(ms) => eprintln!("nevis: text on screen {ms:.1} ms after launch ({in_main:.1} ms in main)"),
            None => eprintln!("nevis: text on screen {in_main:.1} ms after main()"),
        }
    }
    // NEVIS_ACTIONS=win.reading,win.shortcuts: put the window in a state worth a picture, one
    // action at a time as a user would, so a file one of them opens has loaded before the next.
    let actions = std::env::var("NEVIS_ACTIONS").unwrap_or_default();
    let mut actions: Vec<String> = actions.split(',').filter(|a| !a.is_empty()).map(str::to_owned).collect();
    actions.reverse();
    glib::timeout_add_local(Duration::from_millis(150), move || {
        let Some(action) = actions.pop() else {
            finish(window.clone());
            return glib::ControlFlow::Break;
        };
        let (name, argument) = match action.split_once(':') {
            Some((name, argument)) => (name, Some(argument.to_variant())),
            None => (action.as_str(), None),
        };
        if WidgetExt::activate_action(&window, name, argument.as_ref()).is_err() {
            eprintln!("nevis: no action {name}");
        }
        glib::ControlFlow::Continue
    });
}

/// After the actions: typing, the picture and the exit.
fn finish(window: gtk::ApplicationWindow) {
    // Give deferred work (highlighting) time to land before typing, the picture and the exit.
    let wait = std::env::var("NEVIS_WAIT").ok().and_then(|ms| ms.parse().ok()).unwrap_or(500);
    glib::timeout_add_local_once(Duration::from_millis(wait), move || {
        let typed = type_text(&window) | paste_image(&window);
        let settle = std::env::var("NEVIS_SETTLE").ok().and_then(|ms| ms.parse().ok()).unwrap_or(300);
        let settle = Duration::from_millis(if typed { settle } else { 0 });
        // The picture right after a frame is painted (between frames a pending resize can
        // leave the window without a size), or after a second if the compositor isn't
        // drawing this window (another workspace, covered).
        glib::timeout_add_local_once(settle, move || {
            let painted = window.clone();
            let once = Rc::new(Cell::new(true));
            let fallback = once.clone();
            let late = window.clone();
            glib::timeout_add_local_once(Duration::from_secs(1), move || {
                if fallback.replace(false) {
                    finish_picture(&late);
                }
            });
            on_next_paint(&painted, move || {
                if once.replace(false) {
                    finish_picture(&window);
                }
            });
        });
    });
}

/// Saves the picture and quits.
fn finish_picture(window: &gtk::ApplicationWindow) {
    if let Some(path) = std::env::var_os("NEVIS_SNAPSHOT")
        && let Err(err) = snapshot(window, &path)
    {
        eprintln!("nevis: snapshot failed: {err}");
    }
    // Close the windows the normal way, so closing saves what it saves; if a "save changes?"
    // question holds one open, stop anyway shortly after.
    if WidgetExt::activate_action(window, "app.quit", None).is_err() {
        eprintln!("nevis: no app.quit action");
    }
    let app = window.application();
    glib::timeout_add_local_once(Duration::from_millis(300), move || {
        if let Some(app) = app {
            app.quit();
        }
    });
}

/// NEVIS_PASTE_IMAGE: the image pasting takes instead of the clipboard's.
pub fn pasted_image() -> Option<gtk::gdk::Texture> {
    std::env::var_os("NEVIS_PASTE_IMAGE").and_then(|path| gtk::gdk::Texture::from_filename(path).ok())
}

/// NEVIS_PASTE_IMAGE: pastes into the text view with the focus, as Ctrl+V would.
fn paste_image(window: &gtk::ApplicationWindow) -> bool {
    if std::env::var_os("NEVIS_PASTE_IMAGE").is_none() {
        return false;
    }
    let Some(view) = RootExt::focus(window).and_downcast::<gtk::TextView>() else {
        eprintln!("nevis: NEVIS_PASTE_IMAGE: no text view has focus");
        return false;
    };
    view.emit_paste_clipboard();
    true
}

/// NEVIS_TYPE: inserts the text where the focus is (a text view or field), as typing would.
fn type_text(window: &gtk::ApplicationWindow) -> bool {
    let Ok(text) = std::env::var("NEVIS_TYPE") else { return false };
    let focus = RootExt::focus(window);
    if let Some(view) = focus.clone().and_downcast::<gtk::TextView>() {
        let mut rest = text.as_str();
        while !rest.is_empty() {
            let (typed, key) = match rest.find('{').zip(rest.find('}')) {
                Some((open, close)) if open < close => (&rest[..open], Some(&rest[open + 1..close])),
                _ => (rest, None),
            };
            if !typed.is_empty() {
                view.buffer().delete_selection(true, true);
                view.buffer().insert_interactive_at_cursor(typed, true);
            }
            if let Some(key) = key {
                press(&view, key);
            }
            rest = &rest[typed.len() + key.map_or(0, |key| key.len() + 2)..];
        }
    } else if let Some(field) = focus.and_downcast::<gtk::Editable>() {
        let (text, enter) = match text.strip_suffix('\n') {
            Some(text) => (text, true),
            None => (text.as_str(), false),
        };
        field.delete_selection();
        let mut position = field.position();
        field.insert_text(text, &mut position);
        field.set_position(position);
        // The focus is the field's inner GtkText; Enter there emits its "activate".
        if enter {
            field.emit_by_name::<()>("activate", &[]);
        }
    } else {
        eprintln!("nevis: NEVIS_TYPE: nothing to type into has focus");
        return false;
    }
    true
}

/// Presses `key` ("Tab", "S-Tab", "Enter", "End") in `view`: its key controllers first, as
/// GTK would, then the view's own binding for moving to the end.
fn press(view: &gtk::TextView, key: &str) {
    let (keyval, state) = match key {
        "Tab" => (gdk::Key::Tab, gdk::ModifierType::empty()),
        "S-Tab" => (gdk::Key::ISO_Left_Tab, gdk::ModifierType::SHIFT_MASK),
        "Enter" => (gdk::Key::Return, gdk::ModifierType::empty()),
        "End" => {
            let buffer = view.buffer();
            buffer.place_cursor(&buffer.end_iter());
            return;
        }
        _ => return eprintln!("nevis: NEVIS_TYPE: no key {key}"),
    };
    let controllers = view.observe_controllers();
    for controller in (0..controllers.n_items()).filter_map(|i| controllers.item(i).and_downcast::<gtk::EventControllerKey>()) {
        if controller.emit_by_name::<bool>("key-pressed", &[&glib::translate::IntoGlib::into_glib(keyval), &0u32, &state]) {
            return;
        }
    }
    let character = if keyval == gdk::Key::Return { "\n" } else { "\t" };
    view.buffer().insert_interactive_at_cursor(character, true);
}

fn snapshot(window: &gtk::ApplicationWindow, path: &std::ffi::OsStr) -> Result<(), String> {
    let content = window.child().ok_or("window has no content")?;
    // A window the compositor isn't drawing (another workspace) has its layout pending
    // forever: lay it out here, at its last size or else the size it opened at.
    let (width, height) = match (content.width(), content.height()) {
        (0, _) | (_, 0) => window.default_size(),
        size => size,
    };
    content.allocate(width.max(400), height.max(300), -1, None);
    let snap = gtk::Snapshot::new();
    // Draw the content now: copying its last frame gives nothing while a repaint is pending.
    window.snapshot_child(&content, &snap);
    let node = snap.to_node().ok_or("nothing was drawn")?;
    let renderer = window.renderer().ok_or("window has no renderer")?;
    renderer.render_texture(&node, None).save_to_png(path).map_err(|e| e.to_string())
}
