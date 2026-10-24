//! A window: tabs of notes and files (editor.rs), the folder sidebar and the popups over it,
//! and keeping it all from one run to the next.
//!
//! Closing nevis (Ctrl+Q, or closing its last window) asks nothing: every tab is kept, unsaved
//! text included, and comes back the next time nevis opens. Closing a single tab with unsaved
//! changes asks first.
//!
//! The window has no header bar or icons (GTK's first icon lookup costs ~300 ms on startup);
//! the title goes to the compositor, and the menu lives in the right-click menu.

use std::path::PathBuf;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::{gdk, gio, glib, prelude::*};

use crate::editor::{Editor, OnLoad, alert, choose, spawn, tilde};
use crate::outline::Outline;
use crate::palette::Palette;
use crate::picker::Picker;
use crate::prefs::SettingsPage;
use crate::prompt::Prompt;
use crate::search::Search;
use crate::session::{self, Session, WindowState};
use crate::sidebar::Sidebar;
use crate::switcher::Switcher;
use crate::{backlinks, daily, devhooks, help, listing, names, settings, theme, vault};

thread_local! {
    /// Every open window, oldest first.
    static WINDOWS: RefCell<Vec<Rc<Window>>> = const { RefCell::new(Vec::new()) };
    /// A write of the session is due.
    static STORE_DUE: Cell<bool> = const { Cell::new(false) };
    /// The last session's windows, set aside while files opened from outside show on their
    /// own: kept for next time meanwhile, and back with a plain `nevis`.
    static DORMANT: RefCell<Vec<WindowState>> = const { RefCell::new(Vec::new()) };
}

/// Where a file opens.
#[derive(Clone, Copy, PartialEq)]
pub enum How {
    /// In a tab of its own.
    NewTab,
    /// In place of the tab in front, when that has no unsaved changes.
    Replace,
}

pub struct Window {
    pub window: gtk::ApplicationWindow,
    /// Holds `paned`, with the popups (cheat sheet, switcher…) drawn over it.
    overlay: gtk::Overlay,
    /// The folder sidebar (once shown) beside the tabs.
    paned: gtk::Paned,
    /// The tab bar (shown with two tabs or more), scrolled sideways when it overflows, and
    /// the pages under it.
    bar: gtk::Box,
    bar_scroller: gtk::ScrolledWindow,
    pages: gtk::Stack,
    /// The tabs in order, and the one in front.
    editors: RefCell<Vec<Rc<Editor>>>,
    current: RefCell<Option<Rc<Editor>>>,
    /// The folder this window browses, if any, and its sidebar (created on first Ctrl+B).
    folder: RefCell<Option<gio::File>>,
    sidebar: RefCell<Option<Sidebar>>,
    /// Popups, created on first use.
    switcher: RefCell<Option<Rc<Switcher>>>,
    search: RefCell<Option<Rc<Search>>>,
    prompt: RefCell<Option<Rc<Prompt>>>,
    picker: RefCell<Option<Rc<Picker>>>,
    outline: RefCell<Option<Rc<Outline>>>,
    palette: RefCell<Option<Rc<Palette>>>,
    settings: RefCell<Option<Rc<SettingsPage>>>,
    help: RefCell<Option<gtk::Widget>>,
    /// Files of tabs closed lately, for Ctrl+Shift+T.
    closed: RefCell<Vec<gio::File>>,
    /// Closing as nevis quits: every tab is kept for next time, without questions.
    keeping: Cell<bool>,
    /// Focus mode: no tab bar, no sidebar (whether it showed before), the text faded but for
    /// the paragraph being written.
    focused: Cell<bool>,
    sidebar_before: Cell<bool>,
    /// Closing after the user answered for its unsaved changes.
    confirmed: Cell<bool>,
    /// Restoring tabs from the last session: they load when shown, not when added.
    restoring: Cell<bool>,
}

impl Window {
    fn new(app: &gtk::Application) -> Rc<Self> {
        // Not a GtkNotebook: it squeezes tabs to an ellipsis even with room to spare, and its
        // scroll arrows cost ~0.4 s of icon loading when they first show.
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        // After the tabs, "+" for a new one (a label: no icons).
        let new_tab = gtk::Label::builder().label("+").css_classes(["nevis-tab-new"]).tooltip_text("New tab (Ctrl+T)").build();
        let strip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        strip.add_css_class("nevis-tabbar");
        strip.append(&bar);
        strip.append(&new_tab);
        let bar_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&strip)
            .visible(false)
            .build();
        let pages = gtk::Stack::builder().vexpand(true).hexpand(true).hhomogeneous(false).vhomogeneous(false).build();
        let tabbed = gtk::Box::new(gtk::Orientation::Vertical, 0);
        tabbed.append(&bar_scroller);
        tabbed.append(&pages);
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .end_child(&tabbed)
            .resize_start_child(false)
            .shrink_start_child(false)
            .position(240)
            .build();
        let overlay = gtk::Overlay::builder().child(&paned).build();
        let size = window_size::recall();
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .default_width(size.width)
            .default_height(size.height)
            .maximized(size.maximized)
            .title("nevis")
            .child(&overlay)
            .build();
        let this = Rc::new(Self {
            window,
            overlay,
            paned,
            bar,
            bar_scroller,
            pages,
            editors: RefCell::new(Vec::new()),
            current: RefCell::new(None),
            folder: RefCell::new(None),
            sidebar: RefCell::new(None),
            switcher: RefCell::new(None),
            search: RefCell::new(None),
            prompt: RefCell::new(None),
            picker: RefCell::new(None),
            outline: RefCell::new(None),
            palette: RefCell::new(None),
            settings: RefCell::new(None),
            help: RefCell::new(None),
            closed: RefCell::new(Vec::new()),
            keeping: Cell::new(false),
            focused: Cell::new(false),
            sidebar_before: Cell::new(false),
            confirmed: Cell::new(false),
            restoring: Cell::new(false),
        });
        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(&this);
        click.connect_released(move |_, _, _, _| {
            if let Some(this) = weak.upgrade() {
                this.new_tab();
            }
        });
        new_tab.add_controller(click);
        this.install_actions();
        this.connect_signals();
        WINDOWS.with_borrow_mut(|windows| windows.push(this.clone()));
        devhooks::lap("window built");
        this
    }

    /// The folder this window browses.
    pub fn folder(&self) -> Option<gio::File> {
        self.folder.borrow().clone()
    }

    /// The tab in front.
    pub fn current(&self) -> Option<Rc<Editor>> {
        self.current.borrow().clone()
    }

    /// The tabs, in the order they show.
    fn tabs(&self) -> Vec<Rc<Editor>> {
        self.editors.borrow().clone()
    }

    fn index_of(&self, editor: &Editor) -> Option<usize> {
        self.editors.borrow().iter().position(|other| std::ptr::eq(other.as_ref(), editor))
    }

    fn add_tab(self: &Rc<Self>, editor: Rc<Editor>, select: bool) -> Rc<Editor> {
        self.editors.borrow_mut().push(editor.clone());
        self.bar.append(&editor.tab);
        // The page joins the window when the tab is first shown: unseen tabs cost little.
        // A click on the tab brings it to the front (its close button takes its own clicks).
        let click = gtk::GestureClick::new();
        let (this, tab) = (Rc::downgrade(self), Rc::downgrade(&editor));
        click.connect_pressed(move |_, _, _, _| {
            if let (Some(this), Some(tab)) = (this.upgrade(), tab.upgrade()) {
                this.select(&tab);
            }
        });
        editor.tab.add_controller(click);
        // While restoring, the window picks its front tab itself.
        if select || (self.current.borrow().is_none() && !self.restoring.get()) {
            self.select(&editor);
        }
        self.tabs_changed();
        editor
    }

    /// Brings `editor`'s tab to the front.
    fn select(&self, editor: &Rc<Editor>) {
        let previous = self.current.replace(Some(editor.clone()));
        if let Some(previous) = previous.filter(|p| !Rc::ptr_eq(p, editor)) {
            previous.tab.remove_css_class("current");
        }
        editor.tab.add_css_class("current");
        if editor.page.parent().is_none() {
            self.pages.add_child(&editor.page);
        }
        self.pages.set_visible_child(&editor.page);
        self.scroll_to_tab(editor);
        if !self.restoring.get() {
            editor.ensure_loaded();
        }
        self.reading_changed(editor, editor.is_reading());
        if editor.focus_mode() != self.focused.get() {
            editor.set_focus_mode(self.focused.get());
        }
        if let Some(location) = editor.location() {
            self.select_in_sidebar(&location);
        }
        self.show_title_of(editor);
        store_soon();
    }

    /// Scrolls the tab bar so that `editor`'s tab shows, once it is laid out.
    fn scroll_to_tab(&self, editor: &Rc<Editor>) {
        let (scroller, tab) = (self.bar_scroller.clone(), editor.tab.clone());
        glib::idle_add_local_once(move || {
            let Some(bounds) = tab.compute_bounds(&scroller) else { return };
            let adjustment = scroller.hadjustment();
            let (left, right) = (f64::from(bounds.x()), f64::from(bounds.x() + bounds.width()));
            if left < 0.0 {
                adjustment.set_value(adjustment.value() + left);
            } else if right > adjustment.page_size() {
                adjustment.set_value(adjustment.value() + right - adjustment.page_size());
            }
        });
    }

    fn tabs_changed(&self) {
        let editors = self.editors.borrow();
        // A single tab needs no tab bar, unless the settings say otherwise.
        self.bar_scroller.set_visible(!self.focused.get() && (editors.len() > 1 || settings::get().tab_bar_always));
        // Alt+1…8 pick the first eight tabs and Alt+9 the last, so those are the numbers shown.
        for (index, editor) in editors.iter().enumerate() {
            let number = if index < 8 {
                Some(index + 1)
            } else {
                (index == editors.len() - 1).then_some(9)
            };
            editor.set_number(number);
        }
        drop(editors);
        self.update_title();
        store_soon();
    }

    /// Moves the tab in front `by` places along the bar.
    fn move_tab(&self, by: i32) {
        let Some(editor) = self.current() else { return };
        let Some(index) = self.index_of(&editor) else { return };
        self.move_tab_to(&editor, (index as i32 + by).max(0) as usize);
        self.scroll_to_tab(&editor);
    }

    /// Moves `editor`'s tab to place `target` along the bar (the last, if past it).
    fn move_tab_to(&self, editor: &Rc<Editor>, target: usize) {
        let Some(index) = self.index_of(editor) else { return };
        let target = target.min(self.editors.borrow().len() - 1);
        if target == index {
            return;
        }
        let moved = self.editors.borrow_mut().remove(index);
        self.editors.borrow_mut().insert(target, moved);
        // The bar shows the tabs in the list's order.
        let before = if target == 0 { None } else { Some(self.editors.borrow()[target - 1].tab.clone()) };
        self.bar.reorder_child_after(&editor.tab, before.as_ref());
        self.tabs_changed();
    }

    /// The tab at `x`, `y` in the bar.
    fn tab_at(&self, x: f64, y: f64) -> Option<Rc<Editor>> {
        let point = gtk::graphene::Point::new(x as f32, y as f32);
        self.tabs().into_iter().find(|editor| editor.tab.compute_bounds(&self.bar).is_some_and(|bounds| bounds.contains_point(&point)))
    }

    /// Where the middle of the tab at place `index` is, along the bar.
    fn middle_of(&self, index: usize) -> Option<f64> {
        let editor = self.editors.borrow().get(index)?.clone();
        let bounds = editor.tab.compute_bounds(&self.bar)?;
        Some(f64::from(bounds.x() + bounds.width() / 2.0))
    }

    /// A new, empty note in a tab of its own.
    fn new_tab(self: &Rc<Self>) -> Rc<Editor> {
        let editor = self.add_tab(Editor::new(self), true);
        editor.focus_view();
        devhooks::lap("tab added");
        editor
    }

    /// Shows `file` in a tab here (or brings forward the tab that already has it, in any
    /// window), then whatever `then` asks for.
    pub fn open(self: &Rc<Self>, file: gio::File, then: Option<OnLoad>, how: How) {
        if let Some((host, editor)) = find_tab(&file) {
            host.select(&editor);
            host.window.present();
            editor.ensure_loaded();
            if let Some(then) = then {
                editor.arrive(then);
            }
            return;
        }
        match self.current() {
            Some(current) if current.is_pristine() => current.load(file, then),
            Some(current) if how == How::Replace && !current.is_modified() && current.tab_state().draft.is_none() => {
                current.replace_with(file, then);
            }
            _ => self.add_tab(Editor::new(self), true).load(file, then),
        }
        self.window.present();
        store_soon();
    }

    /// Closes a tab, asking first about unsaved changes; the window goes with its last tab.
    pub async fn close_tab(self: Rc<Self>, editor: Rc<Editor>) {
        // Unsaved text from last time that was never shown: show it rather than lose it.
        if editor.tab_state().draft.is_some() && !editor.is_modified() {
            self.select(&editor);
            editor.ensure_loaded();
            return;
        }
        if !editor.clone().may_close().await {
            return;
        }
        editor.closing();
        if let Some(location) = editor.location() {
            self.closed.borrow_mut().push(location);
        }
        let index = self.index_of(&editor);
        self.editors.borrow_mut().retain(|other| !Rc::ptr_eq(other, &editor));
        self.bar.remove(&editor.tab);
        if editor.page.parent().is_some() {
            self.pages.remove(&editor.page);
        }
        let next = index.and_then(|i| {
            let editors = self.editors.borrow();
            editors.get(i).or_else(|| editors.last()).cloned()
        });
        match next {
            None => {
                self.current.replace(None);
                self.window.close();
                return;
            }
            Some(next) if self.current.borrow().as_ref().is_some_and(|c| Rc::ptr_eq(c, &editor)) => self.select(&next),
            Some(_) => {}
        }
        self.tabs_changed();
    }

    /// The tabs with unsaved changes.
    fn unsaved(&self) -> Vec<Rc<Editor>> {
        self.tabs().into_iter().filter(|editor| editor.is_modified() || editor.tab_state().draft.is_some()).collect()
    }

    /// Closing a window while others stay open: what about its unsaved changes?
    async fn confirm_close(self: Rc<Self>, unsaved: Vec<Rc<Editor>>) {
        let message = match unsaved.len() {
            1 => format!("Save changes to “{}”?", unsaved[0].display_name()),
            n => format!("Save changes to {n} notes?"),
        };
        let detail = "Your changes will be lost if you don't save them.";
        let choice = choose(&self.window, &message, detail, &["Cancel", "Don't Save", "Save"]).await;
        match choice {
            Some(2) => {
                for editor in &unsaved {
                    self.select(editor);
                    editor.ensure_loaded();
                    if !editor.clone().save().await {
                        return;
                    }
                }
            }
            Some(1) => {}
            _ => return,
        }
        for editor in &unsaved {
            editor.closing();
        }
        self.confirmed.set(true);
        self.window.close();
    }

    fn install_actions(self: &Rc<Self>) {
        // On the tab in front.
        self.on_current("open", |w, _| spawn(w.clone().open_dialog()));
        self.on_current("save", |_, e| spawn(e.save()));
        self.on_current("save-as", |_, e| spawn(e.save_as()));
        self.on_current("print", |_, e| spawn(e.print(None)));
        self.on_current("export-pdf", |_, e| spawn(e.export_pdf()));
        self.on_current("export-html", |_, e| spawn(e.export_html()));
        self.on_current("side-by-side", |_, e| {
            let on = !e.is_side_by_side();
            e.set_side_by_side(on);
        });
        self.on_current("history", |w, e| {
            let versions = e.versions();
            if versions.is_empty() {
                return alert(&w.window, "No earlier versions yet", "nevis keeps a version each time a note is saved (one every ten minutes at most).");
            }
            if let Some(palette) = w.palette_popup() {
                palette.show(versions, "Go back to the version from…");
            }
        });
        self.with_path("restore-version", |w, version| {
            if let (Some(editor), Some(path)) = (w.current(), version.path()) {
                editor.restore_version(&path);
            }
        });
        self.on_current("find", |_, e| {
            e.set_reading(false);
            e.find_bar().show(false);
        });
        self.on_current("replace", |_, e| {
            e.set_reading(false);
            e.find_bar().show(true);
        });
        self.on_current("find-next", |_, e| {
            e.set_reading(false);
            e.find_bar().next();
        });
        self.on_current("find-prev", |_, e| {
            e.set_reading(false);
            e.find_bar().prev();
        });
        self.on_current("close", |w, e| spawn(w.clone().close_tab(e)));
        let reading = gio::SimpleAction::new_stateful("reading", None, &false.to_variant());
        let weak = Rc::downgrade(self);
        reading.connect_activate(move |action, _| {
            let on = !action.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            if let Some(editor) = weak.upgrade().and_then(|w| w.current()) {
                editor.set_reading(on);
            }
        });
        self.window.add_action(&reading);
        // win.follow(url): follows a link as clicking it in the rendered view would.
        self.with_text("follow", |w, url| {
            if let Some(editor) = w.current() {
                editor.open_link(&url, false);
            }
        });

        // Tabs.
        self.action("new-tab", |w| {
            w.new_tab();
        });
        self.action("close-window", |w| w.window.close());
        self.action("reopen-tab", |w| {
            let last = w.closed.borrow_mut().pop();
            if let Some(file) = last {
                w.open(file, None, How::NewTab);
            }
        });
        self.action("move-tab-left", |w| w.move_tab(-1));
        self.action("move-tab-right", |w| w.move_tab(1));
        self.action("next-tab", |w| w.step_tab(1));
        self.action("prev-tab", |w| w.step_tab(-1));
        for n in 1..=9 {
            self.action(&format!("tab-{n}"), move |w| {
                let editors = w.tabs();
                // Alt+9 is the last tab, as in browsers.
                let pick = if n == 9 { editors.last() } else { editors.get(n - 1) };
                if let Some(editor) = pick {
                    w.select(editor);
                }
            });
        }

        // The window.
        self.action("zoom-in", |_| zoom(1.0));
        self.action("zoom-out", |_| zoom(-1.0));
        self.action("zoom-reset", |_| zoom(0.0));
        self.toggle("wrap", settings::get().wrap, |_, on| settings::change(|s| s.wrap = on));
        self.toggle("line-numbers", settings::get().line_numbers, |_, on| settings::change(|s| s.line_numbers = on));
        self.action("settings", |w| w.toggle_settings());
        self.action("shortcuts", |w| w.toggle_help());
        self.action("sidebar", |w| w.toggle_sidebar());
        self.toggle("focus", false, |w, on| w.set_focus_mode(on));
        self.action("theme", |w| w.pick_theme());
        self.action("outline", |w| w.toggle_outline());
        self.action("palette", |w| w.toggle_palette());
        self.action("daily", |w| w.open_daily());
        self.action("link", |w| {
            if let Some(editor) = w.current() {
                editor.make_link();
            }
        });
        self.with_path("switch-to", |w, file| w.open(file, None, How::Replace));
        self.with_path("note-new", |w, folder| w.ask_new_note(folder, None));
        self.action("new-from-template", |w| w.pick_template());
        self.with_path("note-from-template", |w, template| {
            if let Some(folder) = w.note_folder() {
                w.ask_new_note(folder, Some(template));
            }
        });
        self.with_path("note-rename", |w, file| w.ask_rename(file));
        self.with_path("note-trash", |w, file| spawn(w.clone().trash(file)));
        self.with_path("note-reveal", |w, file| {
            gtk::FileLauncher::new(Some(&file)).open_containing_folder(Some(&w.window), None::<&gio::Cancellable>, |_| {});
        });
        // A named note in a folder window; elsewhere a new tab.
        self.action("new-note", |w| match w.note_folder().filter(|_| w.folder.borrow().is_some()) {
            Some(folder) => w.ask_new_note(folder, None),
            None => {
                w.new_tab();
            }
        });
        self.action("open-folder", |w| spawn(w.clone().open_folder_dialog()));
        self.action("quick-open", |w| w.quick_open());
        self.action("search", |w| w.search_folder());
    }

    fn action(self: &Rc<Self>, name: &str, run: impl Fn(&Rc<Self>) + 'static) {
        let action = gio::SimpleAction::new(name, None);
        let weak = Rc::downgrade(self);
        action.connect_activate(move |_, _| {
            if let Some(this) = weak.upgrade() {
                run(&this);
            }
        });
        self.window.add_action(&action);
    }

    /// An action on the tab in front.
    fn on_current(self: &Rc<Self>, name: &str, run: impl Fn(&Rc<Self>, Rc<Editor>) + 'static) {
        self.action(name, move |this| {
            if let Some(editor) = this.current() {
                run(this, editor);
            }
        });
    }

    /// An action taking a string (a URL, a path).
    fn with_text(self: &Rc<Self>, name: &str, run: impl Fn(&Rc<Self>, String) + 'static) {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        action.connect_activate(move |_, text| {
            if let (Some(this), Some(text)) = (weak.upgrade(), text.and_then(|t| t.get::<String>())) {
                run(&this, text);
            }
        });
        self.window.add_action(&action);
    }

    /// An action taking a file path (the sidebar menu's, and `switch-to`).
    fn with_path(self: &Rc<Self>, name: &str, run: impl Fn(&Rc<Self>, gio::File) + 'static) {
        self.with_text(name, move |this, path| run(this, gio::File::for_path(path)));
    }

    fn toggle(self: &Rc<Self>, name: &str, initial: bool, run: impl Fn(&Rc<Self>, bool) + 'static) {
        let action = gio::SimpleAction::new_stateful(name, None, &initial.to_variant());
        let weak = Rc::downgrade(self);
        action.connect_activate(move |action, _| {
            let on = !action.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            action.set_state(&on.to_variant());
            if let Some(this) = weak.upgrade() {
                run(&this, on);
            }
        });
        self.window.add_action(&action);
    }

    fn step_tab(&self, by: i32) {
        let editors = self.tabs();
        let count = editors.len() as i32;
        if count > 1 {
            let current = self.current().and_then(|c| self.index_of(&c)).unwrap_or(0) as i32;
            self.select(&editors[(current + by).rem_euclid(count) as usize]);
        }
    }

    fn connect_signals(self: &Rc<Self>) {
        // A tab dragged along the bar takes the place of each one whose middle it passes (and
        // can't flip back and forth over a wide one). The × takes its own clicks.
        let drag = gtk::GestureDrag::new();
        let dragged: Rc<RefCell<Option<Rc<Editor>>>> = Rc::default();
        let (weak, held) = (Rc::downgrade(self), dragged.clone());
        drag.connect_drag_begin(move |_, x, y| {
            *held.borrow_mut() = weak.upgrade().and_then(|this| this.tab_at(x, y));
        });
        let (weak, held) = (Rc::downgrade(self), dragged.clone());
        drag.connect_drag_update(move |gesture, dx, _| {
            let (Some(this), Some(editor)) = (weak.upgrade(), held.borrow().clone()) else { return };
            let threshold = gtk::Settings::default().map_or(8, |settings| settings.gtk_dnd_drag_threshold());
            let Some((start, _)) = gesture.start_point() else { return };
            if dx.abs() < f64::from(threshold) {
                return;
            }
            let Some(index) = this.index_of(&editor) else { return };
            let x = start + dx;
            if this.middle_of(index + 1).is_some_and(|middle| x > middle) {
                this.move_tab_to(&editor, index + 1);
            } else if index > 0 && this.middle_of(index - 1).is_some_and(|middle| x < middle) {
                this.move_tab_to(&editor, index - 1);
            }
        });
        drag.connect_drag_end(move |_, _, _| {
            dragged.borrow_mut().take();
        });
        self.bar.add_controller(drag);
        // The mouse wheel scrolls an overflowing tab bar sideways.
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let scroller = self.bar_scroller.downgrade();
        wheel.connect_scroll(move |_, dx, dy| {
            if let Some(scroller) = scroller.upgrade() {
                let adjustment = scroller.hadjustment();
                adjustment.set_value(adjustment.value() + (dx + dy) * 40.0);
            }
            glib::Propagation::Stop
        });
        self.bar_scroller.add_controller(wheel);

        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |_| {
            let Some(this) = weak.upgrade() else { return glib::Propagation::Proceed };
            if this.keeping.get() || this.confirmed.get() {
                return glib::Propagation::Proceed;
            }
            // The last window: everything is kept for next time, nothing asked.
            if WINDOWS.with_borrow(|windows| windows.len()) <= 1 {
                keep_everything();
                return glib::Propagation::Proceed;
            }
            let unsaved = this.unsaved();
            if unsaved.is_empty() {
                return glib::Propagation::Proceed;
            }
            spawn(this.clone().confirm_close(unsaved));
            glib::Propagation::Stop
        });

        // While the cheat sheet is open, a key closes it instead of reaching the text.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(this) = weak.upgrade() else { return glib::Propagation::Proceed };
            if !this.help_open() || is_modifier(key) {
                return glib::Propagation::Proceed;
            }
            this.hide_help();
            glib::Propagation::Stop
        });
        self.window.add_controller(keys);

        // The tabs' numbers (for Alt+1 … Alt+9) show while Alt is held.
        let alt = |key: gdk::Key| matches!(key, gdk::Key::Alt_L | gdk::Key::Alt_R);
        let numbers = gtk::EventControllerKey::new();
        numbers.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        numbers.connect_key_pressed(move |_, key, _, _| {
            if alt(key) && let Some(this) = weak.upgrade() {
                this.show_numbers(true);
            }
            glib::Propagation::Proceed
        });
        let weak = Rc::downgrade(self);
        numbers.connect_key_released(move |_, key, _, _| {
            if alt(key) && let Some(this) = weak.upgrade() {
                this.show_numbers(false);
            }
        });
        self.window.add_controller(numbers);
        let weak = Rc::downgrade(self);
        self.window.connect_is_active_notify(move |window| {
            if !window.is_active() && let Some(this) = weak.upgrade() {
                this.show_numbers(false);
            }
        });
    }

    fn show_numbers(&self, on: bool) {
        for editor in self.tabs() {
            editor.show_number(on);
        }
    }

    /// The tab in front switched between the rendered view and the source.
    pub fn reading_changed(&self, editor: &Editor, on: bool) {
        if !self.is_current(editor) {
            return;
        }
        if let Some(action) = self.window.lookup_action("reading").and_downcast::<gio::SimpleAction>() {
            action.set_state(&on.to_variant());
        }
    }

    fn is_current(&self, editor: &Editor) -> bool {
        self.current.borrow().as_ref().is_some_and(|current| std::ptr::eq(current.as_ref(), editor))
    }

    /// A tab's title changed (name, unsaved changes).
    pub fn title_changed(&self, editor: &Editor) {
        if self.is_current(editor) {
            self.show_title_of(editor);
        }
    }

    fn update_title(&self) {
        match self.current() {
            Some(editor) => self.show_title_of(&editor),
            None => {
                let title = self.folder().map_or_else(|| "nevis".to_owned(), |folder| tilde(&folder.parse_name()));
                self.window.set_title(Some(&title));
            }
        }
    }

    /// "• notes.md — ~/Documents"
    fn show_title_of(&self, editor: &Editor) {
        let mut title = editor.title();
        if let Some(folder) = editor.location().and_then(|l| l.parent()).or_else(|| self.folder()) {
            title.push_str(&format!(" — {}", tilde(&folder.parse_name())));
        }
        self.window.set_title(Some(&title));
    }

    /// Focuses the text of the tab in front.
    fn focus_view(&self) {
        if let Some(editor) = self.current() {
            editor.focus_view();
        }
    }

    fn set_focus_mode(&self, on: bool) {
        self.focused.set(on);
        if let Some(sidebar) = self.sidebar.borrow().as_ref() {
            if on {
                self.sidebar_before.set(sidebar.widget.is_visible());
                sidebar.widget.set_visible(false);
            } else {
                sidebar.widget.set_visible(self.sidebar_before.get());
            }
        }
        self.tabs_changed();
        // On: the tab in front (the others when they come forward, see `select`). Off: all.
        if on {
            if let Some(editor) = self.current() {
                editor.set_focus_mode(true);
            }
        } else {
            for editor in self.tabs().iter().filter(|editor| editor.focus_mode()) {
                editor.set_focus_mode(false);
            }
        }
    }

    /// Shows or hides the sidebar; the first time, for this window's folder or its file's.
    fn toggle_sidebar(self: &Rc<Self>) {
        if let Some(sidebar) = self.sidebar.borrow().as_ref() {
            sidebar.widget.set_visible(!sidebar.widget.is_visible());
            return;
        }
        if let Some(folder) = self.browsed_folder() {
            self.show_folder(&folder);
        }
    }

    /// The folder this window browses, or else the one its file is in.
    fn browsed_folder(&self) -> Option<gio::File> {
        self.folder().or_else(|| self.current().and_then(|e| e.location()).and_then(|l| l.parent()))
    }

    fn show_folder(self: &Rc<Self>, folder: &gio::File) {
        *self.folder.borrow_mut() = Some(folder.clone());
        let (opener, menu) = (Rc::downgrade(self), Rc::downgrade(self));
        let sidebar = Sidebar::new(
            folder,
            move |file| {
                if let Some(this) = opener.upgrade() {
                    this.open(file, None, if ctrl_held() { How::NewTab } else { How::Replace });
                }
            },
            move |file, is_folder, row| {
                if let Some(this) = menu.upgrade() {
                    this.row_menu(&file, is_folder, row);
                }
            },
        );
        self.paned.set_start_child(Some(&sidebar.widget));
        if let Some(location) = self.current().and_then(|e| e.location()) {
            sidebar.select(&location);
        }
        *self.sidebar.borrow_mut() = Some(sidebar);
        self.update_title();
        store_soon();
    }

    pub fn select_in_sidebar(&self, location: &gio::File) {
        if let Some(sidebar) = self.sidebar.borrow().as_ref() {
            sidebar.select(location);
        }
    }

    /// Opens the search through the notes of this window's folder on `text` (a tag chosen).
    pub fn search_for(self: &Rc<Self>, text: &str) {
        let Some(root) = self.browsed_folder() else { return };
        let search = self.search_popup();
        search.set_query(text);
        if !search.is_open() {
            search.show(&root);
        }
    }

    /// Opens (or closes) the search through the notes of this window's folder, or its file's.
    fn search_folder(self: &Rc<Self>) {
        let Some(root) = self.browsed_folder() else { return };
        let search = self.search_popup();
        if search.is_open() {
            search.hide();
        } else {
            search.show(&root);
        }
    }

    /// The folder search popup, made the first time.
    fn search_popup(self: &Rc<Self>) -> Rc<Search> {
        let existing = self.search.borrow().clone();
        match existing {
            Some(search) => search,
            None => {
                let weak = Rc::downgrade(self);
                let search = Search::new(move |file, line, in_line| {
                    if let Some(this) = weak.upgrade() {
                        let how = if ctrl_held() { How::NewTab } else { How::Replace };
                        this.open(file, Some(OnLoad::Hit { line, in_line }), how);
                    }
                });
                self.add_popup(&search.widget);
                *self.search.borrow_mut() = Some(search.clone());
                search
            }
        }
    }

    /// Opens (or closes) the quick switcher on this window's folder, or its file's.
    fn quick_open(self: &Rc<Self>) {
        let Some(root) = self.browsed_folder() else { return };
        let existing = self.switcher.borrow().clone();
        let switcher = match existing {
            Some(switcher) => switcher,
            None => {
                let weak = Rc::downgrade(self);
                let tagged = weak.clone();
                let switcher = Switcher::new(
                    move |file, heading| {
                        if let Some(this) = weak.upgrade() {
                            this.open(file, heading.map(OnLoad::Heading), if ctrl_held() { How::NewTab } else { How::Replace });
                        }
                    },
                    move |tag| {
                        if let Some(this) = tagged.upgrade() {
                            this.search_for(&tag);
                        }
                    },
                );
                self.add_popup(&switcher.widget);
                *self.switcher.borrow_mut() = Some(switcher.clone());
                switcher
            }
        };
        if switcher.is_open() {
            switcher.hide();
        } else {
            switcher.show(&root);
        }
    }

    /// Opens (or closes) the Settings page.
    fn toggle_settings(self: &Rc<Self>) {
        let existing = self.settings.borrow().clone();
        let page = match existing {
            Some(page) => page,
            None => {
                let weak = Rc::downgrade(self);
                let page = SettingsPage::new(move || {
                    if let Some(this) = weak.upgrade() {
                        this.pick_theme();
                    }
                });
                self.add_popup(&page.widget);
                *self.settings.borrow_mut() = Some(page.clone());
                page
            }
        };
        if page.is_open() {
            page.hide();
        } else {
            page.show();
        }
    }

    /// Settings changed: what shows in this window follows, the Settings page included.
    fn settings_changed(&self) {
        let now = settings::get();
        for (name, on) in [("wrap", now.wrap), ("line-numbers", now.line_numbers)] {
            if let Some(action) = self.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                action.set_state(&on.to_variant());
            }
        }
        if let Some(page) = self.settings.borrow().as_ref() {
            page.refresh();
        }
        self.tabs_changed();
    }

    /// Opens (or closes) the theme picker.
    /// Opens today's note in the folder, making it first if there is none yet.
    fn open_daily(self: &Rc<Self>) {
        let Some(root) = self.folder() else {
            return alert(&self.window, "Daily notes go in a folder", "Open a folder first (Ctrl+Shift+O), then press Ctrl+D again.");
        };
        let none = None::<&gio::Cancellable>;
        let obsidian = root
            .resolve_relative_path(".obsidian/daily-notes.json")
            .load_contents(none)
            .ok()
            .map(|(bytes, _)| String::from_utf8_lossy(&bytes).into_owned());
        let subfolders: Vec<String> = root
            .enumerate_children("standard::name,standard::type", gio::FileQueryInfoFlags::NONE, none)
            .map(|children| {
                children
                    .flatten()
                    .filter(|info| info.file_type() == gio::FileType::Directory)
                    .map(|info| info.name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let subfolders: Vec<&str> = subfolders.iter().map(String::as_str).collect();
        let Ok(now) = glib::DateTime::now_local() else { return };
        let today = (now.year(), now.month() as u32, now.day_of_month() as u32);
        let file = root.resolve_relative_path(daily::path(today, obsidian.as_deref(), &subfolders));
        if !file.query_exists(none) {
            if let Some(folder) = file.parent() {
                let _ = folder.make_directory_with_parents(none);
            }
            if let Err(error) = file.create(gio::FileCreateFlags::NONE, none) {
                return alert(&self.window, "Couldn't make today's note", error.message());
            }
            vault::forget();
        }
        self.open(file, None, How::NewTab);
    }

    fn toggle_outline(self: &Rc<Self>) {
        let Some(editor) = self.current() else { return };
        let existing = self.outline.borrow().clone();
        let outline = match existing {
            Some(outline) => outline,
            None => {
                let weak = Rc::downgrade(self);
                let outline = Outline::new(move |index, headings| {
                    if let Some(editor) = weak.upgrade().and_then(|this| this.current()) {
                        editor.go_to_heading(index, headings);
                    }
                });
                self.add_popup(&outline.widget);
                *self.outline.borrow_mut() = Some(outline.clone());
                outline
            }
        };
        if outline.is_open() {
            return outline.hide();
        }
        let headings = editor.outline();
        let current = editor.current_heading(&headings);
        outline.show(headings, current);
    }

    /// The command palette: every command that can run now, to run one by its name.
    fn toggle_palette(self: &Rc<Self>) {
        let palette = self.palette();
        if palette.is_open() {
            return palette.hide();
        }
        let Some(app) = self.window.application() else { return };
        // Those that can run now: an action off (Save with nothing to save, say) isn't offered.
        let can_run = |action: &str| match action.split_once('.') {
            Some(("win", name)) => self.window.lookup_action(name).is_some_and(|action| action.is_enabled()),
            Some(("app", name)) => app.lookup_action(name).is_some_and(|action| action.is_enabled()),
            _ => false,
        };
        let commands = crate::help::commands(&app).into_iter().filter(|command| command.action != "win.palette" && can_run(&command.action)).collect();
        palette.show(commands, "Run a command");
    }

    /// The palette, made on first use, toggled: the open one closes instead (`None`).
    fn palette_popup(self: &Rc<Self>) -> Option<Rc<Palette>> {
        let palette = self.palette();
        if palette.is_open() {
            palette.hide();
            return None;
        }
        Some(palette)
    }

    /// The command palette, made on first use; it runs detailed action names.
    fn palette(self: &Rc<Self>) -> Rc<Palette> {
        if let Some(palette) = self.palette.borrow().clone() {
            return palette;
        }
        let window = self.window.downgrade();
        let palette = Palette::new(move |action| {
            if let Some(window) = window.upgrade()
                && let Ok((name, target)) = gio::Action::parse_detailed_name(action)
            {
                let _ = WidgetExt::activate_action(&window, &name, target.as_ref());
            }
        });
        self.add_popup(&palette.widget);
        *self.palette.borrow_mut() = Some(palette.clone());
        palette
    }

    /// The notes in the folder's templates folder, to start a new note from (with its name
    /// asked for next).
    fn pick_template(self: &Rc<Self>) {
        let Some(root) = self.folder() else {
            return alert(&self.window, "Templates are in a folder", "Open a folder first (Ctrl+Shift+O), with your templates in its templates folder.");
        };
        let none = None::<&gio::Cancellable>;
        let obsidian = root
            .resolve_relative_path(".obsidian/templates.json")
            .load_contents(none)
            .ok()
            .map(|(bytes, _)| String::from_utf8_lossy(&bytes).into_owned());
        let subfolders = child_names(&root);
        let subfolders: Vec<&str> = subfolders.iter().map(String::as_str).collect();
        let templates: Vec<gio::File> = crate::templates::folder(obsidian.as_deref(), &subfolders)
            .map(|folder| root.resolve_relative_path(folder))
            .map(|folder| {
                let mut names = child_names(&folder);
                names.retain(|name| listing::is_note(name));
                names.sort_by_key(|name| name.to_lowercase());
                names.iter().map(|name| folder.child(name)).collect()
            })
            .unwrap_or_default();
        if templates.is_empty() {
            return alert(&self.window, "No templates yet", "Put notes in a folder called templates (or the one Obsidian's Templates settings name), then try again.");
        }
        let commands = templates
            .iter()
            .filter_map(|template| {
                let path = template.path()?;
                let name = template.basename()?.to_string_lossy().into_owned();
                let what = std::path::Path::new(&name).file_stem().map_or(name.clone(), |stem| stem.to_string_lossy().into_owned());
                let action = gio::Action::print_detailed_name("win.note-from-template", Some(&path.to_string_lossy().to_variant()));
                Some(crate::help::Command { what, action: action.to_string(), keys: String::new() })
            })
            .collect();
        if let Some(palette) = self.palette_popup() {
            palette.show(commands, "New note from template");
        }
    }

    fn pick_theme(self: &Rc<Self>) {
        let existing = self.picker.borrow().clone();
        let picker = match existing {
            Some(picker) => picker,
            None => {
                let picker = Picker::new();
                self.add_popup(&picker.widget);
                *self.picker.borrow_mut() = Some(picker.clone());
                picker
            }
        };
        if picker.is_open() {
            picker.hide();
        } else {
            picker.show();
        }
    }

    /// Puts a popup over the window; the text gets the focus back when it closes.
    fn add_popup(self: &Rc<Self>, widget: &gtk::Box) {
        let weak = Rc::downgrade(self);
        widget.connect_visible_notify(move |widget| {
            if !widget.is_visible()
                && let Some(this) = weak.upgrade()
            {
                this.focus_view();
            }
        });
        self.overlay.add_overlay(widget);
    }

    /// The menu for a sidebar row: new note there, rename, trash, show in the file manager.
    fn row_menu(&self, file: &gio::File, is_folder: bool, row: &gtk::Widget) {
        let Some(path) = file.path().map(|p| p.to_string_lossy().into_owned()) else { return };
        let here = if is_folder { Some(path.clone()) } else { file.parent().and_then(|p| p.path()).map(|p| p.to_string_lossy().into_owned()) };
        let item = |label: &str, action: &str, target: &str| {
            let item = gio::MenuItem::new(Some(label), None);
            item.set_action_and_target_value(Some(action), Some(&target.to_variant()));
            item
        };
        let menu = gio::Menu::new();
        if let Some(here) = here {
            menu.append_item(&item("New Note Here…", "win.note-new", &here));
        }
        let changes = gio::Menu::new();
        changes.append_item(&item("Rename…", "win.note-rename", &path));
        changes.append_item(&item("Move to Trash", "win.note-trash", &path));
        menu.append_section(None, &changes);
        let more = gio::Menu::new();
        more.append_item(&item("Show in File Manager", "win.note-reveal", &path));
        menu.append_section(None, &more);

        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_parent(row);
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        // Unparent once closed, after the menu has finished with it.
        popover.connect_closed(|popover| {
            let popover = popover.clone();
            glib::idle_add_local_once(move || popover.unparent());
        });
        popover.popup();
    }

    /// The folder new notes go in: the open note's, or the window's.
    fn note_folder(&self) -> Option<gio::File> {
        self.current().and_then(|e| e.location()).and_then(|l| l.parent()).or_else(|| self.folder())
    }

    /// The name prompt, created on first use.
    fn prompt(self: &Rc<Self>) -> Rc<Prompt> {
        if let Some(prompt) = self.prompt.borrow().as_ref() {
            return prompt.clone();
        }
        let prompt = Prompt::new();
        self.add_popup(&prompt.widget);
        *self.prompt.borrow_mut() = Some(prompt.clone());
        prompt
    }

    /// Asks for a name, creates the note in `folder`, and opens it to type into.
    /// Asks for a new note's name, and makes it in `folder`: empty, or from `template`.
    fn ask_new_note(self: &Rc<Self>, folder: gio::File, template: Option<gio::File>) {
        let suggestion = names::new_note(&child_names(&folder));
        let (checked, weak) = (folder.clone(), Rc::downgrade(self));
        self.prompt().ask(
            "New note",
            &suggestion,
            move |input| {
                let name = names::chosen(input).map_err(str::to_owned)?;
                match checked.child(&name).query_exists(None::<&gio::Cancellable>) {
                    true => Err("A file with that name already exists.".to_owned()),
                    false => Ok(name),
                }
            },
            move |name| {
                let Some(this) = weak.upgrade() else { return };
                let file = folder.child(&name);
                let none = None::<&gio::Cancellable>;
                let text = template.as_ref().and_then(|template| template.load_contents(none).ok()).map(|(bytes, _)| {
                    let title = std::path::Path::new(&name).file_stem().map_or(name.clone(), |stem| stem.to_string_lossy().into_owned());
                    let now = glib::DateTime::now_local().ok();
                    let date = now.as_ref().map_or((1970, 1, 1), |now| (now.year(), now.month() as u32, now.day_of_month() as u32));
                    let time = now.as_ref().map_or((0, 0), |now| (now.hour() as u32, now.minute() as u32));
                    crate::templates::fill(&String::from_utf8_lossy(&bytes), &title, date, time)
                });
                match file.create(gio::FileCreateFlags::NONE, none) {
                    Ok(stream) => {
                        if let Some(text) = &text {
                            let _ = stream.write_all(text.as_bytes(), none);
                        }
                        let _ = stream.close(none);
                        vault::forget();
                        this.open(file, Some(OnLoad::Edit), How::NewTab);
                    }
                    Err(err) => alert(&this.window, "Could not create the note", err.message()),
                }
            },
        );
    }

    /// Asks for a new name for a file or folder and renames it; open files follow along.
    fn ask_rename(self: &Rc<Self>, file: gio::File) {
        let current = file.basename().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let is_folder =
            file.query_file_type(gio::FileQueryInfoFlags::NONE, None::<&gio::Cancellable>) == gio::FileType::Directory;
        let (parent, original, unchanged, weak) = (file.parent(), current.clone(), current.clone(), Rc::downgrade(self));
        self.prompt().ask(
            "Rename",
            &current,
            move |input| {
                let name = if is_folder { names::chosen_folder(input) } else { names::chosen(input) };
                let name = name.map_err(str::to_owned)?;
                let taken = parent.as_ref().is_some_and(|p| p.child(&name).query_exists(None::<&gio::Cancellable>));
                if name != original && taken { Err("A file with that name already exists.".to_owned()) } else { Ok(name) }
            },
            move |name| {
                let Some(this) = weak.upgrade() else { return };
                if name == unchanged {
                    return;
                }
                // A note's links are found while it still has its old name.
                let window = this.folder().and_then(|folder| folder.path());
                let linked = (!is_folder && listing::is_note(&unchanged)).then(|| vault_of(&file, window.as_deref())).flatten();
                match file.set_display_name(&name, None::<&gio::Cancellable>) {
                    Ok(renamed) => {
                        vault::forget();
                        for open in editors_at_or_under(&file) {
                            let Some(location) = open.location() else { continue };
                            match file.relative_path(&location) {
                                Some(below) => open.moved_to(&renamed.resolve_relative_path(below)),
                                None => open.moved_to(&renamed),
                            }
                        }
                        if let Some((root, old, files)) = linked {
                            let new = old.rsplit_once('/').map_or(name.clone(), |(folders, _)| format!("{folders}/{name}"));
                            spawn(this.clone().update_links(root, old, new, files));
                        }
                    }
                    Err(err) => alert(&this.window, "Could not rename", err.message()),
                }
            },
        );
    }

    /// After a note is renamed: offers to point the links to it at its new name. An open note
    /// with unsaved changes gets the change in its tab; the others are rewritten on disk.
    async fn update_links(self: Rc<Self>, root: PathBuf, old: String, new: String, files: Vec<String>) {
        let (searched, wanted, listed) = (root.clone(), old.clone(), files.clone());
        // Links by an alias still lead to the note under its new name: those stay as they are.
        let Ok(found) = gio::spawn_blocking(move || backlinks::find(&searched, &wanted, &listed, &[])).await else { return };
        if found.is_empty() {
            return;
        }
        let name = |path: &str| path.rsplit('/').next().unwrap_or(path).trim_end_matches(".md").to_owned();
        let message = format!("Update the links to “{}”?", name(&old));
        let detail = match found.len() {
            1 => format!("1 note links to it. Its links can point at “{}” instead.", name(&new)),
            count => format!("{count} notes link to it. Their links can point at “{}” instead.", name(&new)),
        };
        if choose(&self.window, &message, &detail, &["Leave Them", "Update Links"]).await != Some(1) {
            return;
        }
        for (path, _) in found {
            let on_disk = root.join(&path);
            let open = editors_at_or_under(&gio::File::for_path(&on_disk)).into_iter().find(|editor| editor.is_modified());
            match open {
                Some(editor) => {
                    if let Some(text) = vault::relink(&editor.text(), &path, &old, &new, &files) {
                        editor.replace_text(&text);
                    }
                }
                None => {
                    let Ok(text) = std::fs::read_to_string(&on_disk) else { continue };
                    if let Some(text) = vault::relink(&text, &path, &old, &new, &files) {
                        let _ = std::fs::write(&on_disk, text);
                    }
                }
            }
        }
        vault::forget();
    }

    /// Moves a file or folder to the trash, after asking.
    async fn trash(self: Rc<Self>, file: gio::File) {
        let name = file.basename().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let message = format!("Move “{name}” to the trash?");
        let choice = choose(&self.window, &message, "You can restore it from the trash.", &["Cancel", "Move to Trash"]).await;
        if choice != Some(1) {
            return;
        }
        match file.trash(None::<&gio::Cancellable>) {
            Ok(()) => {
                vault::forget();
                editors_at_or_under(&file).iter().for_each(|open| open.check_disk());
            }
            Err(err) => alert(&self.window, "Could not move it to the trash", err.message()),
        }
    }

    async fn open_dialog(self: Rc<Self>) {
        let dialog = gtk::FileDialog::builder().title("Open").modal(true).build();
        if let Some(folder) = self.current().and_then(|e| e.location()).and_then(|l| l.parent()).or_else(|| self.folder()) {
            dialog.set_initial_folder(Some(&folder));
        }
        if let Ok(file) = dialog.open_future(Some(&self.window)).await {
            self.open(file, None, How::NewTab);
        }
    }

    async fn open_folder_dialog(self: Rc<Self>) {
        let dialog = gtk::FileDialog::builder().title("Open Folder").modal(true).build();
        if let Some(folder) = self.browsed_folder() {
            dialog.set_initial_folder(Some(&folder));
        }
        if let Ok(folder) = dialog.select_folder_future(Some(&self.window)).await {
            self.show_folder(&folder);
        }
    }

    fn toggle_help(self: &Rc<Self>) {
        if self.help_open() {
            return self.hide_help();
        }
        if self.help.borrow().is_none() {
            let Some(app) = self.window.application() else { return };
            let weak = Rc::downgrade(self);
            let sheet = help::build(&app, move || {
                if let Some(this) = weak.upgrade() {
                    this.hide_help();
                }
            });
            self.overlay.add_overlay(&sheet);
            *self.help.borrow_mut() = Some(sheet);
        }
        if let Some(sheet) = self.help.borrow().as_ref() {
            sheet.set_visible(true);
        }
    }

    fn help_open(&self) -> bool {
        self.help.borrow().as_ref().is_some_and(|sheet| sheet.is_visible())
    }

    fn hide_help(&self) {
        if let Some(sheet) = self.help.borrow().as_ref() {
            sheet.set_visible(false);
        }
        self.focus_view();
    }

    /// How this window is to be found next time.
    fn state(&self) -> WindowState {
        let tabs = self.tabs();
        let active = self.current().and_then(|c| self.index_of(&c)).unwrap_or(0);
        WindowState {
            folder: self.folder().and_then(|f| f.path()),
            active,
            tabs: tabs.iter().map(|editor| editor.tab_state()).collect(),
        }
    }
}

/// A plain `nevis`: the notes as they were left (the last session, set aside if files were
/// opened before it), else the window in front, else an empty note.
pub fn activate(app: &gtk::Application) {
    let set_aside = DORMANT.take();
    if !set_aside.is_empty() {
        restore(app, set_aside, true);
        return;
    }
    match WINDOWS.with_borrow(|windows| windows.last().cloned()) {
        Some(window) => window.window.present(),
        None => {
            let window = restore_or_new(app);
            if window.current().is_some_and(|editor| editor.is_pristine()) {
                devhooks::content_ready(&window.window);
            }
        }
    }
}

/// Files and folders opened from outside (a file manager, a terminal): they open where you
/// are, in a window of their own; several in one go share it, as tabs. A file already open
/// comes forward instead. If nevis wasn't open, the last session is set aside rather than
/// shown, still kept, until a plain `nevis` brings it back.
pub fn open_files(app: &gtk::Application, files: &[gio::File]) {
    devhooks::lap("open asked");
    let in_tabs = !settings::get().outside_in_window;
    let nothing_open = WINDOWS.with_borrow(|windows| windows.is_empty());
    if nothing_open && in_tabs {
        // Set to open files in tabs: the last session comes back, and they join it.
        restore_or_new(app);
    } else if nothing_open {
        set_aside_last_session();
    }
    let (folders, files): (Vec<&gio::File>, Vec<&gio::File>) = files.iter().partition(|file| {
        file.query_file_type(gio::FileQueryInfoFlags::NONE, None::<&gio::Cancellable>) == gio::FileType::Directory
    });
    // The window this call fills: set to tabs, the one in front; otherwise the one in front
    // when all it has is a new, empty note, or else a new one.
    let mut target: Option<Rc<Window>> = None;
    let fresh = || {
        let reusable = |window: &Rc<Window>| {
            in_tabs || (window.folder().is_none() && window.tabs().len() == 1 && window.tabs()[0].is_pristine())
        };
        front(app).filter(reusable).unwrap_or_else(|| Window::new(app))
    };
    for file in files {
        if let Some((host, editor)) = find_tab(file) {
            host.select(&editor);
            host.window.present();
            continue;
        }
        let window = target.get_or_insert_with(&fresh);
        window.open(file.clone(), None, How::NewTab);
    }
    for folder in folders {
        let window = match target.as_ref().filter(|window| window.folder().is_none()) {
            Some(window) => window.clone(),
            None => Some(fresh()).filter(|window| window.folder().is_none()).unwrap_or_else(|| Window::new(app)),
        };
        window.show_folder(folder);
        if window.tabs().is_empty() {
            window.new_tab();
            devhooks::content_ready(&window.window);
        }
        target.get_or_insert(window);
    }
    devhooks::lap("window ready");
    if let Some(window) = target {
        window.window.present();
    } else if WINDOWS.with_borrow(|windows| windows.is_empty()) {
        // Only files open elsewhere were asked for, and nothing is: nothing to show? An empty
        // note, as a plain `nevis` gives.
        activate(app);
    }
}

/// A new window, with an empty note.
pub fn new_window(app: &gtk::Application) {
    if WINDOWS.with_borrow(|windows| windows.is_empty()) {
        set_aside_last_session();
    }
    let window = Window::new(app);
    window.new_tab();
    window.window.present();
    devhooks::content_ready(&window.window);
}

/// With no window open, the session on disk is how nevis was left: set aside, kept, so a new
/// window's session doesn't replace it (and drop its drafts).
fn set_aside_last_session() {
    if DORMANT.with_borrow(|dormant| dormant.is_empty()) && session::enabled() {
        DORMANT.set(session::load().windows);
    }
}

/// Ctrl+Q: every window closes, every tab is kept for next time.
pub fn quit() {
    keep_everything();
    let windows = WINDOWS.with_borrow(|windows| windows.clone());
    for window in windows {
        window.window.close();
    }
}

/// The window in front, if nevis has one.
fn front(app: &gtk::Application) -> Option<Rc<Window>> {
    let active = app.active_window();
    WINDOWS.with_borrow(|windows| {
        windows
            .iter()
            .find(|w| active.as_ref().is_some_and(|a| w.window.upcast_ref::<gtk::Window>() == a))
            .or(windows.last())
            .cloned()
    })
}

/// The windows of the last session, or a window with an empty note.
fn restore_or_new(app: &gtk::Application) -> Rc<Window> {
    let windows = if session::enabled() { session::load().windows } else { Vec::new() };
    restore(app, windows, true).unwrap_or_else(|| {
        let window = Window::new(app);
        window.new_tab();
        window.window.present();
        window
    })
}

/// Brings `windows` back as one window of tabs (`session::merged`), each tab loading when
/// first shown (`load`: the tab in front now). Tabs already open in another window aren't
/// doubled. The window, if any.
fn restore(app: &gtk::Application, windows: Vec<WindowState>, load: bool) -> Option<Rc<Window>> {
    let state = session::merged(windows)?;
    // A file deleted since comes back only if it has unsaved text.
    let keep = |tab: &session::TabState| {
        (tab.draft.is_some() || tab.path.as_ref().is_some_and(|p| p.exists()))
            && tab.path.as_ref().is_none_or(|p| find_tab(&gio::File::for_path(p)).is_none())
    };
    let (mut active, mut tabs) = (0, Vec::new());
    for (i, tab) in state.tabs.into_iter().enumerate() {
        if keep(&tab) {
            if i <= state.active {
                active = tabs.len();
            }
            tabs.push(tab);
        }
    }
    if tabs.is_empty() {
        return None;
    }
    let window = Window::new(app);
    window.restoring.set(true);
    if let Some(folder) = state.folder {
        window.show_folder(&gio::File::for_path(folder));
    }
    for tab in tabs {
        window.add_tab(Editor::restored(&window, tab), false);
    }
    if let Some(editor) = window.tabs().get(active).cloned() {
        window.select(&editor);
    }
    window.restoring.set(false);
    if load && let Some(editor) = window.current() {
        editor.ensure_loaded();
    }
    window.window.present();
    Some(window)
}

/// Writes the session soon; edits and tab changes come in bursts.
pub fn store_soon() {
    if !session::enabled() || STORE_DUE.replace(true) {
        return;
    }
    glib::timeout_add_local_once(Duration::from_millis(500), || {
        STORE_DUE.set(false);
        // With no window left, the session written on the way out stands.
        if WINDOWS.with_borrow(|windows| !windows.is_empty()) {
            session::store(&snapshot());
        }
    });
}

/// Every window and tab as they are now.
fn snapshot() -> Session {
    let mut windows: Vec<WindowState> = WINDOWS.with_borrow(|windows| windows.iter().map(|w| w.state()).collect());
    windows.extend(DORMANT.with_borrow(|dormant| dormant.clone()));
    Session { windows }
}

/// nevis is closing: drafts of unsaved text written now, and the session with them.
fn keep_everything() {
    let windows = WINDOWS.with_borrow(|windows| windows.clone());
    for window in &windows {
        window.keeping.set(true);
        for editor in window.tabs() {
            editor.keep_draft_now();
        }
    }
    // With no window left (the last one closed, then nevis was stopped, as logging out does),
    // the session written when it closed stands: an empty one would drop its drafts.
    let session = snapshot();
    if session::enabled() && !session.windows.is_empty() {
        session::store(&session);
    }
    // All of it is on disk now, set-aside windows included.
    DORMANT.take();
}

/// Lets go of a window that left the app (it was closed).
///
/// Not done on "destroy": GTK only emits that once the last reference is gone, and the
/// window itself holds one.
pub fn forget(window: &gtk::Window) {
    let gone = WINDOWS.with_borrow_mut(|windows| {
        let index = windows.iter().position(|w| w.window.upcast_ref::<gtk::Window>() == window)?;
        Some(windows.remove(index))
    });
    // Outside the borrow: dropping widgets may run handlers.
    if let Some(gone) = &gone {
        for editor in gone.tabs() {
            editor.remember_place();
        }
        let (width, height) = gone.window.default_size();
        window_size::keep(window_size::Size { width, height, maximized: gone.window.is_maximized() });
        if !gone.keeping.get() {
            store_soon();
        }
    }
    drop(gone);
}

/// Repaints every tab when the colours change, and applies changed settings everywhere.
/// Call once at startup.
pub fn init() {
    theme::on_change(|| all_editors().iter().for_each(|editor| editor.refresh(true)));
    settings::on_change(|old, new| {
        if old.text_size != new.text_size {
            theme::apply_font();
        }
        // Rendered views again: list indents and diagrams follow the font; lines join or not.
        if old.text_size != new.text_size || old.strict_line_breaks != new.strict_line_breaks {
            all_editors().iter().for_each(|editor| editor.refresh(false));
        }
        if new.spell_check && !old.spell_check && !crate::spelling::available() {
            let detail = "Install it and a dictionary or two, then turn this on again:\n\nsudo pacman -S libspelling hunspell-en_us hunspell-it";
            if let Some(window) = WINDOWS.with_borrow(|windows| windows.last().cloned()) {
                alert(&window.window, "Spell checking needs libspelling", detail);
            }
            return settings::change(|s| s.spell_check = false);
        }
        for editor in all_editors() {
            editor.set_wrap(new.wrap);
            // Also fits the margins to the text width and size.
            editor.set_line_numbers(new.line_numbers && !editor.focus_mode());
            if new.spell_check != old.spell_check {
                editor.set_spell_check(new.spell_check);
            }
            if new.live_preview != old.live_preview {
                editor.set_live_preview(new.live_preview);
            }
        }
        let windows = WINDOWS.with_borrow(|windows| windows.clone());
        windows.iter().for_each(|window| window.settings_changed());
    });
}

/// Changes the text size by `steps` points; 0 resets it.
pub fn zoom(steps: f64) {
    theme::zoom(steps);
}

fn all_editors() -> Vec<Rc<Editor>> {
    WINDOWS.with_borrow(|windows| windows.iter().flat_map(|w| w.tabs()).collect())
}

/// The tab showing `file`, and its window.
fn find_tab(file: &gio::File) -> Option<(Rc<Window>, Rc<Editor>)> {
    WINDOWS.with_borrow(|windows| {
        windows.iter().find_map(|window| {
            let editor = window.tabs().into_iter().find(|e| e.location().is_some_and(|l| l.equal(file)))?;
            Some((window.clone(), editor))
        })
    })
}

/// The tabs showing `file`, or a file inside it when it is a folder.
fn editors_at_or_under(file: &gio::File) -> Vec<Rc<Editor>> {
    all_editors()
        .into_iter()
        .filter(|e| e.location().is_some_and(|l| l.equal(file) || l.has_prefix(file)))
        .collect()
}

/// The vault `file` is in, its path there, and the vault's files: what renaming it needs to
/// find the links to it.
fn vault_of(file: &gio::File, window: Option<&std::path::Path>) -> Option<(PathBuf, String, Vec<String>)> {
    let path = file.path()?;
    let root = vault::root_of(path.parent()?, window);
    let old = path.strip_prefix(&root).ok()?.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
    Some((root.clone(), old, vault::files(&root).as_ref().clone()))
}

/// The names in `folder`.
fn child_names(folder: &gio::File) -> Vec<String> {
    let Ok(children) = folder.enumerate_children("standard::name", gio::FileQueryInfoFlags::NONE, None::<&gio::Cancellable>)
    else {
        return Vec::new();
    };
    children.flatten().map(|info| info.name().to_string_lossy().into_owned()).collect()
}

/// Whether Ctrl is down right now: Ctrl+click and Ctrl+Enter open in a new tab.
fn ctrl_held() -> bool {
    gdk::Display::default()
        .and_then(|display| display.default_seat())
        .and_then(|seat| seat.keyboard())
        .is_some_and(|keyboard| keyboard.modifier_state().contains(gdk::ModifierType::CONTROL_MASK))
}

/// Shift, Ctrl, Alt, Super…: pressed on their own, they shouldn't close the cheat sheet,
/// since they start the Ctrl+? that may be closing it.
fn is_modifier(key: gdk::Key) -> bool {
    use gdk::Key;
    matches!(
        key,
        Key::Shift_L | Key::Shift_R | Key::Control_L | Key::Control_R | Key::Alt_L | Key::Alt_R
            | Key::Super_L | Key::Super_R | Key::Meta_L | Key::Meta_R | Key::ISO_Level3_Shift
            | Key::Caps_Lock
    )
}

thread_local! {
    static MENU: gio::Menu = build_menu();
}

/// Appended to the right-click menu of both views (one model, shared by every tab).
pub fn menu_model() -> gio::Menu {
    MENU.with(|menu| menu.clone())
}

fn build_menu() -> gio::Menu {
    let section = |items: &[(&str, &str)]| {
        let menu = gio::Menu::new();
        for (label, action) in items {
            menu.append(Some(label), Some(action));
        }
        menu
    };
    // Only what's about the text at hand and the window's files; preferences live on the
    // Settings page.
    let menu = gio::Menu::new();
    menu.append_section(None, &section(&[
        ("New Tab", "win.new-tab"),
        ("Open…", "win.open"),
        ("Open Folder…", "win.open-folder"),
        ("Save", "win.save"),
        ("Save As…", "win.save-as"),
        ("Export as PDF…", "win.export-pdf"),
        ("Export as HTML…", "win.export-html"),
        ("Print…", "win.print"),
    ]));
    menu.append_section(None, &section(&[("Find…", "win.find"), ("Reading View", "win.reading"), ("Side by Side", "win.side-by-side")]));
    menu.append_section(None, &section(&[("Settings…", "win.settings"), ("Keyboard Shortcuts", "win.shortcuts")]));
    menu
}

/// The size of the last window closed, which new windows start at.
mod window_size {
    use std::cell::Cell;

    #[derive(Clone, Copy)]
    pub struct Size {
        pub width: i32,
        pub height: i32,
        pub maximized: bool,
    }

    const DEFAULT: Size = Size { width: 860, height: 640, maximized: false };

    thread_local! {
        static LAST: Cell<Option<Size>> = const { Cell::new(None) };
    }

    pub fn recall() -> Size {
        LAST.get().unwrap_or_else(|| {
            let size = read().unwrap_or(DEFAULT);
            LAST.set(Some(size));
            size
        })
    }

    pub fn keep(size: Size) {
        if size.width <= 0 || size.height <= 0 {
            return;
        }
        LAST.set(Some(size));
        let path = crate::positions::state_dir().join("window");
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, format!("{} {} {}\n", size.width, size.height, size.maximized));
    }

    /// "width height maximized" from the state file.
    fn read() -> Option<Size> {
        let text = std::fs::read_to_string(crate::positions::state_dir().join("window")).ok()?;
        let mut fields = text.split_whitespace();
        let width = fields.next()?.parse().ok()?;
        let height = fields.next()?.parse().ok()?;
        let maximized = fields.next()?.parse().ok()?;
        (width > 0 && height > 0).then_some(Size { width, height, maximized })
    }
}
