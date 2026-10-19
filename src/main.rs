//! nevis: a fast, minimal text and Markdown editor and notepad.

mod anchors;
mod attachments;
mod classes;
mod backlinks;
mod color;
mod complete;
mod daily;
mod devhooks;
mod editor;
mod find;
mod flowchart;
mod gantt;
mod gtkcolors;
mod help;
mod links;
mod lists;
mod listing;
mod math;
mod names;
mod obsidian;
mod outline;
mod palette;
mod palettes;
mod picker;
mod popup;
mod positions;
mod prefs;
mod print;
mod paper;
mod prompt;
mod render;
mod search;
mod sequence;
mod session;
mod settings;
mod spelling;
mod state;
mod sidebar;
mod style;
mod switcher;
mod theme;
mod vault;
mod window;

use gtk::{gio, glib, prelude::*};

const APP_ID: &str = "dev.shirvani.Nevis";

fn main() -> glib::ExitCode {
    devhooks::mark_start();

    // One process serves every window, so opening a second file skips GTK startup.
    // Measurement runs get their own process instead of handing off to a running one.
    let mut flags = gio::ApplicationFlags::HANDLES_OPEN;
    if devhooks::active() {
        flags |= gio::ApplicationFlags::NON_UNIQUE;
    }
    // NEVIS_APP_ID: run a separate instance (tests), leaving the real service alone.
    let app_id = std::env::var("NEVIS_APP_ID").unwrap_or_else(|_| APP_ID.to_owned());
    let app = gtk::Application::builder().application_id(app_id).flags(flags).build();

    app.connect_startup(|app| {
        devhooks::lap("gtk ready");
        sourceview::init();
        theme::init();
        window::init();
        devhooks::lap("theme ready");
        install_actions(app);
        app.connect_window_removed(|_, window| window::forget(window));
        on_stop_signals();
        // Started as a background service (`nevis --gapplication-service`, see nevis.service):
        // stay running with no windows, so every file opens without GTK's ~0.5 s startup.
        if app.flags().contains(gio::ApplicationFlags::IS_SERVICE) {
            std::mem::forget(app.hold());
            // A little after login, so it doesn't compete with everything else starting.
            glib::timeout_add_local_once(std::time::Duration::from_secs(2), editor::warm_up);
        }
    });
    app.connect_activate(window::activate);
    app.connect_open(|app, files, _hint| window::open_files(app, files));
    app.run()
}

/// Stopped from outside (logging out, `systemctl --user restart nevis`, Ctrl+C in a terminal):
/// keep every tab and its unsaved text first, as Ctrl+Q does.
fn on_stop_signals() {
    // GLib's own, from the library nevis already uses (glib-rs moved it to another crate).
    unsafe extern "C" {
        fn g_unix_signal_add(signum: std::ffi::c_int, handler: glib::ffi::GSourceFunc, data: glib::ffi::gpointer) -> std::ffi::c_uint;
    }
    // Runs on the main loop, like any other event.
    unsafe extern "C" fn stop(_: glib::ffi::gpointer) -> glib::ffi::gboolean {
        window::quit();
        if let Some(app) = gio::Application::default() {
            app.quit();
        }
        glib::ffi::GFALSE
    }
    const SIGINT: std::ffi::c_int = 2;
    const SIGTERM: std::ffi::c_int = 15;
    for signal in [SIGINT, SIGTERM] {
        // SAFETY: `stop` matches GSourceFunc and uses no data; GLib keeps the source.
        unsafe { g_unix_signal_add(signal, Some(stop), std::ptr::null_mut()) };
    }
}

fn install_actions(app: &gtk::Application) {
    let new_window = gio::SimpleAction::new("new-window", None);
    let weak = app.downgrade();
    new_window.connect_activate(move |_, _| {
        if let Some(app) = weak.upgrade() {
            window::new_window(&app);
        }
    });
    app.add_action(&new_window);

    // Every window closes; every tab, unsaved text included, is kept for next time.
    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate(|_, _| window::quit());
    app.add_action(&quit);

    for (action, keys) in [
        ("app.new-window", &["<Ctrl><Shift>n"][..]),
        ("win.new-note", &["<Ctrl>n"]),
        ("app.quit", &["<Ctrl>q"]),
        ("win.open", &["<Ctrl>o"]),
        ("win.save", &["<Ctrl>s"]),
        ("win.save-as", &["<Ctrl><Shift>s"]),
        ("win.export-pdf", &["<Ctrl><Shift>e"]),
        ("win.print", &["<Ctrl><Alt>p"]),
        ("win.close", &["<Ctrl>w"]),
        ("win.close-window", &["<Ctrl><Shift>w"]),
        ("win.new-tab", &["<Ctrl>t"]),
        ("win.reopen-tab", &["<Ctrl><Shift>t"]),
        ("win.next-tab", &["<Ctrl>Tab", "<Ctrl>Page_Down"]),
        ("win.move-tab-left", &["<Ctrl><Shift>Page_Up"]),
        ("win.move-tab-right", &["<Ctrl><Shift>Page_Down"]),
        ("win.prev-tab", &["<Ctrl><Shift>Tab", "<Ctrl><Shift>ISO_Left_Tab", "<Ctrl>Page_Up"]),
        ("win.tab-1", &["<Alt>1"]),
        ("win.tab-2", &["<Alt>2"]),
        ("win.tab-3", &["<Alt>3"]),
        ("win.tab-4", &["<Alt>4"]),
        ("win.tab-5", &["<Alt>5"]),
        ("win.tab-6", &["<Alt>6"]),
        ("win.tab-7", &["<Alt>7"]),
        ("win.tab-8", &["<Alt>8"]),
        ("win.tab-9", &["<Alt>9"]),
        ("win.settings", &["<Ctrl>comma"]),
        ("win.outline", &["<Ctrl>r"]),
        ("win.daily", &["<Ctrl>d"]),
        ("win.link", &["<Ctrl>k"]),
        ("win.find", &["<Ctrl>f"]),
        ("win.replace", &["<Ctrl>h"]),
        ("win.find-next", &["F3", "<Ctrl>g"]),
        ("win.find-prev", &["<Shift>F3", "<Ctrl><Shift>g"]),
        ("win.zoom-in", &["<Ctrl>plus", "<Ctrl>equal", "<Ctrl>KP_Add"]),
        ("win.zoom-out", &["<Ctrl>minus", "<Ctrl>KP_Subtract"]),
        ("win.zoom-reset", &["<Ctrl>0", "<Ctrl>KP_0"]),
        ("win.wrap", &["<Alt>z"]),
        ("win.reading", &["<Ctrl>e"]),
        ("win.line-numbers", &["<Ctrl><Shift>l"]),
        ("win.shortcuts", &["<Ctrl>question", "F1"]),
        ("win.sidebar", &["<Ctrl>b"]),
        ("win.open-folder", &["<Ctrl><Shift>o"]),
        ("win.quick-open", &["<Ctrl>p"]),
        ("win.palette", &["<Ctrl><Shift>p"]),
        ("win.search", &["<Ctrl><Shift>f"]),
    ] {
        app.set_accels_for_action(action, keys);
    }
}
