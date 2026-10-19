//! One tab: a text file or an unsaved note, its two views (the source, and Markdown rendered),
//! loading, saving, watching the file on disk, and the draft that keeps unsaved text from one
//! run to the next.
//!
//! The window around it (tabs, sidebar, menus, closing) is in window.rs.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::ops::Range;
use std::rc::{Rc, Weak};
use std::time::Duration;

use gtk::{gdk, gio, glib, prelude::*};
use sourceview::prelude::*;

use crate::render::{self, Target};
use crate::session::{self, TabState};
use crate::window::{How, Window};
use crate::complete::{self, Completion, Context, Offer};
use crate::spelling::Spelling;
use crate::{anchors, attachments, backlinks, daily, devhooks, find::FindBar, links, listing, lists, names, obsidian, outline, positions, print, settings, style, switcher, theme, vault};

pub struct Editor {
    /// The window the tab is in: dialogs, frames and the title go there.
    pub window: gtk::ApplicationWindow,
    host: Weak<Window>,
    /// The tab's page: the find bar (once used) above the two views.
    pub page: gtk::Box,
    /// The tab itself: the title, and a close button that shows a dot while there are unsaved
    /// changes (and the pointer isn't on the tab).
    pub tab: gtk::Box,
    /// The number Alt+number picks the tab by, before the title.
    tab_number: gtk::Label,
    tab_title: gtk::Label,
    tab_close: gtk::Label,
    hovered: Cell<bool>,
    /// Holds the source editor ("edit") and the rendered view ("read").
    stack: gtk::Stack,
    view: crate::paper::Paper,
    pub buffer: sourceview::Buffer,
    /// Styles Markdown in the source view.
    styler: Rc<style::Styler>,
    reader: crate::paper::Paper,
    /// Location, encoding and line endings of the file on disk.
    file: sourceview::File,
    /// Watches the file for changes made by other programs.
    monitor: RefCell<Option<gio::FileMonitor>>,
    /// A look at the file on disk is already scheduled.
    checking: Cell<bool>,
    /// The user kept their version over a newer one on disk; don't ask again until the next
    /// save or reload.
    kept_mine: Cell<bool>,
    /// Created on first Ctrl+F.
    find: RefCell<Option<Rc<FindBar>>>,
    /// Shown in the title after the file name, e.g. "new file".
    note: RefCell<Option<String>>,
    /// Clickable links and checkboxes in the rendered view.
    targets: RefCell<Vec<(gtk::TextTag, Target)>>,
    /// The rendered view's headings, for `#heading` links.
    headings: RefCell<Vec<(String, gtk::TextMark)>>,
    /// Where each block of the note starts in the rendered view, by its line.
    lines: RefCell<Vec<(usize, gtk::TextMark)>>,
    /// Itself, for work that finishes later (finding backlinks).
    me: Weak<Editor>,
    /// The notes found linking here last: the folder, this note, and each linking note with
    /// its lines.
    backlinks: RefCell<Option<Backlinks>>,
    /// Counts the searches for backlinks, so that only the latest one shows.
    backlink_searches: Cell<u32>,
    /// What to show once the file being loaded is in, when not the usual.
    on_load: RefCell<Option<OnLoad>>,
    /// The rendered view is up to date with the source.
    rendered: Cell<bool>,
    /// A question about this file is on screen.
    asking: Cell<bool>,
    saving: Cell<bool>,
    /// The draft keeping unsaved text for next time (a name in the drafts folder), and whether
    /// writing it is due.
    draft: RefCell<Option<String>>,
    draft_due: Cell<bool>,
    /// The save waiting for typing to stop, with autosave on.
    autosave_due: RefCell<Option<glib::SourceId>>,
    /// Spell checking, made the first time it is turned on.
    spelling: RefCell<Option<Spelling>>,
    /// What the tab shows over its views: the completion list while typing.
    layer: gtk::Overlay,
    completion: RefCell<Option<Rc<Completion>>>,
    /// Where the completion closed with Esc started: it stays closed there.
    dismissed: Cell<Option<i32>>,
    /// A look at what is being typed is waiting for the edit (or move) to finish.
    completing: Cell<bool>,
    /// Hovering a link to a note in the rendered view: the note waited for or shown, the wait,
    /// and the popover its start shows in.
    previewing: RefCell<Option<gio::File>>,
    preview_due: RefCell<Option<glib::SourceId>>,
    preview: RefCell<Option<gtk::Popover>>,
    /// The rendered view's side margin (wider with centred text), and a re-render waiting for
    /// the window to stop changing size.
    reader_margin: Cell<i32>,
    refit_due: RefCell<Option<glib::SourceId>>,
    /// Margins wait for the end of a resize, not to change in the middle of laying out.
    fit_queued: Cell<bool>,
    /// From the last session, not loaded yet: that happens the first time the tab is shown.
    restore: RefCell<Option<TabState>>,
}

/// What to show once a file has loaded, instead of the usual (the rendered view for
/// Markdown, the cursor otherwise).
pub enum OnLoad {
    /// The source view, to type into: a note just created.
    Edit,
    /// The rendered view, scrolled to this heading: a "Note#Heading" link.
    Heading(String),
    /// The source view, with the text found on this line selected: a folder search hit.
    Hit { line: usize, in_line: Range<usize> },
    /// As the tab was left last time, unsaved text included.
    Restore { text: Option<String>, line: u32, column: u32, reading: bool },
}

/// What a click in the source view follows.
enum Clicked {
    Link(String),
    Tag(String),
}

impl Editor {
    pub fn new(host: &Rc<Window>) -> Rc<Self> {
        let buffer = sourceview::Buffer::new(None);
        buffer.set_highlight_matching_brackets(false);
        buffer.set_style_scheme(theme::scheme().as_ref());
        let styler = style::Styler::new(&buffer);

        let view = crate::paper::Paper::new();
        view.set_buffer(Some(&buffer));
        styler.draw_on(&view);
        view.add_css_class("nevis-editor");
        let preferences = settings::get();
        view.set_wrap_mode(if preferences.wrap { gtk::WrapMode::WordChar } else { gtk::WrapMode::None });
        view.set_left_margin(16);
        view.set_right_margin(16);
        // Lines are spaced above only: GTK aborts on a click or a drag in the space under a
        // line holding hidden text (Live Preview's markup; see `Paper::iter_at`).
        view.set_top_margin(8);
        view.set_bottom_margin(12);
        view.set_pixels_above_lines(4);
        view.set_pixels_inside_wrap(2);
        view.set_highlight_current_line(true);
        view.set_auto_indent(true);
        view.set_insert_spaces_instead_of_tabs(true);
        view.set_tab_width(4);
        view.set_smart_backspace(true);
        view.set_show_line_numbers(preferences.line_numbers);
        view.set_extra_menu(Some(&crate::window::menu_model()));

        let reader = crate::paper::Paper::new();
        reader.add_css_class("nevis-read");
        reader.set_editable(false);
        reader.set_cursor_visible(false);
        reader.set_wrap_mode(gtk::WrapMode::WordChar);
        reader.set_left_margin(render::MARGIN);
        reader.set_right_margin(render::MARGIN);
        reader.set_top_margin(18);
        reader.set_bottom_margin(28);
        reader.set_pixels_inside_wrap(3);
        reader.set_extra_menu(Some(&crate::window::menu_model()));

        let edit_scroller = gtk::ScrolledWindow::builder().child(&view).build();
        let read_scroller = gtk::ScrolledWindow::builder().child(&reader).build();
        let stack = gtk::Stack::builder().vexpand(true).hexpand(true).hhomogeneous(false).vhomogeneous(false).build();
        stack.add_named(&edit_scroller, Some("edit"));
        stack.add_named(&read_scroller, Some("read"));
        // The tab's views, and over them the completion list while typing.
        let layer = gtk::Overlay::builder().child(&stack).vexpand(true).build();
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.append(&layer);

        // The tab: its number, its title, and a close button (a plain "×": icons cost startup
        // time). Ellipsized, so that many tabs share the bar the way browser tabs do.
        let tab_number = gtk::Label::builder().css_classes(["nevis-tab-number"]).visible(false).build();
        let tab_title = gtk::Label::builder()
            .max_width_chars(24)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["nevis-tab-title"])
            .build();
        // A label rather than a button: lighter, and a window may have many tabs.
        let close = gtk::Label::builder().label("×").css_classes(["nevis-tab-close"]).tooltip_text("Close tab (Ctrl+W)").build();
        let tab = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        tab.add_css_class("nevis-tab");
        tab.append(&tab_number);
        tab.append(&tab_title);
        tab.append(&close);

        let editor = Rc::new_cyclic(|me| Self {
            me: me.clone(),
            window: host.window.clone(),
            host: Rc::downgrade(host),
            page,
            tab,
            tab_number,
            tab_title,
            tab_close: close.clone(),
            hovered: Cell::new(false),
            stack,
            view,
            buffer,
            styler,
            reader,
            file: sourceview::File::new(),
            monitor: RefCell::new(None),
            checking: Cell::new(false),
            kept_mine: Cell::new(false),
            find: RefCell::new(None),
            note: RefCell::new(None),
            targets: RefCell::new(Vec::new()),
            headings: RefCell::new(Vec::new()),
            lines: RefCell::new(Vec::new()),
            backlinks: RefCell::new(None),
            backlink_searches: Cell::new(0),
            on_load: RefCell::new(None),
            rendered: Cell::new(false),
            asking: Cell::new(false),
            saving: Cell::new(false),
            draft: RefCell::new(None),
            draft_due: Cell::new(false),
            autosave_due: RefCell::new(None),
            spelling: RefCell::new(None),
            layer,
            completion: RefCell::new(None),
            dismissed: Cell::new(None),
            completing: Cell::new(false),
            previewing: RefCell::new(None),
            preview_due: RefCell::new(None),
            preview: RefCell::new(None),
            reader_margin: Cell::new(render::MARGIN),
            refit_due: RefCell::new(None),
            fit_queued: Cell::new(false),
            restore: RefCell::new(None),
        });
        editor.connect_signals(&close);
        zoom_on_ctrl_scroll(&edit_scroller);
        zoom_on_ctrl_scroll(&read_scroller);
        // A new note is Markdown until saved as something else.
        editor.styler.set_enabled(true);
        editor.update_tab();
        editor
    }

    /// A tab from the last session, loaded the first time it is shown.
    pub fn restored(host: &Rc<Window>, state: TabState) -> Rc<Self> {
        let editor = Editor::new(host);
        if let Some(path) = &state.path {
            editor.file.set_location(Some(&gio::File::for_path(path)));
        }
        *editor.restore.borrow_mut() = Some(state);
        editor.update_tab();
        editor
    }

    fn connect_signals(self: &Rc<Self>, close: &gtk::Label) {
        // A change of width (resizing, the sidebar): the margins follow, once it is laid out.
        for view in [self.view.upcast_ref::<gtk::TextView>(), self.reader.upcast_ref::<gtk::TextView>()] {
            let Some(adjustment) = view.hadjustment() else { continue };
            let weak = Rc::downgrade(self);
            adjustment.connect_page_size_notify(move |_| {
                let Some(editor) = weak.upgrade() else { return };
                if editor.fit_queued.replace(true) {
                    return;
                }
                let me = editor.me.clone();
                glib::idle_add_local_once(move || {
                    if let Some(editor) = me.upgrade() {
                        editor.fit_queued.set(false);
                        editor.fit_width();
                    }
                });
            });
        }
        // Right-click, with spell checking on: corrections for the word under the pointer.
        let corrections = gtk::GestureClick::builder()
            .button(gdk::BUTTON_SECONDARY)
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        let weak = Rc::downgrade(self);
        corrections.connect_pressed(move |_, _, x, y| {
            let Some(editor) = weak.upgrade().filter(|_| settings::get().spell_check) else { return };
            let spelling = editor.spelling.borrow();
            let Some(spelling) = spelling.as_ref() else { return };
            if !editor.buffer.has_selection() {
                let (bx, by) = editor.view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
                if let Some(iter) = editor.view.iter_at(bx, by) {
                    editor.buffer.place_cursor(&iter);
                }
            }
            spelling.update_corrections();
        });
        self.view.add_controller(corrections);
        // Spell checking, if on, once the tab is first drawn: loading it never delays a file.
        if settings::get().spell_check {
            let weak = Rc::downgrade(self);
            after_next_frame(&self.window, move || {
                if let Some(editor) = weak.upgrade() {
                    editor.set_spell_check(settings::get().spell_check);
                }
            });
        }
        // Pasting in a Markdown note: an image copied (a screenshot) with no text is saved as a
        // file and linked; a web address pasted over selected words links them.
        let weak = Rc::downgrade(self);
        self.view.connect_paste_clipboard(move |view| {
            let Some(editor) = weak.upgrade().filter(|editor| editor.styler.is_enabled()) else { return };
            if let Some(texture) = devhooks::pasted_image() {
                view.stop_signal_emission_by_name("paste-clipboard");
                return editor.paste_image(&texture);
            }
            let clipboard = view.clipboard();
            let formats = clipboard.formats();
            let text = formats.contains_type(glib::Type::STRING);
            if formats.contains_type(gdk::Texture::static_type()) && !text {
                view.stop_signal_emission_by_name("paste-clipboard");
                glib::spawn_future_local(async move {
                    if let Ok(Some(texture)) = clipboard.read_texture_future().await {
                        editor.paste_image(&texture);
                    }
                });
            } else if text && editor.buffer.has_selection() {
                view.stop_signal_emission_by_name("paste-clipboard");
                glib::spawn_future_local(async move {
                    let pasted = clipboard.read_text_future().await.ok().flatten();
                    match pasted.as_deref().map(str::trim).filter(|text| links::is_web_address(text)) {
                        Some(address) => editor.link_selection(address),
                        None => editor.buffer.paste_clipboard(&clipboard, None, true),
                    }
                });
            }
        });
        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(self);
        click.connect_pressed(|click, _, _, _| {
            click.set_state(gtk::EventSequenceState::Claimed);
        });
        click.connect_released(move |_, _, _, _| {
            if let Some(editor) = weak.upgrade()
                && let Some(host) = editor.host.upgrade()
            {
                spawn(host.close_tab(editor));
            }
        });
        close.add_controller(click);
        // A middle click on the tab closes it too.
        let middle = gtk::GestureClick::builder().button(gdk::BUTTON_MIDDLE).build();
        let weak = Rc::downgrade(self);
        middle.connect_released(move |_, _, _, _| {
            if let Some(editor) = weak.upgrade()
                && let Some(host) = editor.host.upgrade()
            {
                spawn(host.close_tab(editor));
            }
        });
        self.tab.add_controller(middle);
        // Over the tab, the unsaved dot turns into the close button it stands in for.
        let hover = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(self);
        hover.connect_enter(move |_, _, _| {
            if let Some(editor) = weak.upgrade() {
                editor.hovered.set(true);
                editor.update_tab();
            }
        });
        let weak = Rc::downgrade(self);
        hover.connect_leave(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.hovered.set(false);
                editor.update_tab();
            }
        });
        self.tab.add_controller(hover);

        let weak = Rc::downgrade(self);
        self.buffer.connect_modified_changed(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.modified_changed();
            }
        });

        let weak = Rc::downgrade(self);
        self.buffer.connect_changed(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.rendered.set(false);
                if editor.buffer.is_modified() {
                    editor.queue_draft();
                    editor.queue_autosave();
                }
                editor.queue_completion();
                // An unsaved note is named after its first line.
                if editor.location().is_none() {
                    editor.update_tab();
                }
            }
        });

        // Rendered view: click a link or checkbox to follow it, double-click to edit.
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        let weak = Rc::downgrade(self);
        click.connect_released(move |gesture, presses, x, y| {
            let Some(editor) = weak.upgrade() else { return };
            editor.hover(None, 0.0, 0.0);
            if presses == 2 {
                editor.set_reading(false);
            } else if !editor.reader.buffer().has_selection()
                && let Some(target) = editor.target_at(x, y)
            {
                let new_tab = gesture.current_event_state().contains(gdk::ModifierType::CONTROL_MASK);
                editor.follow(target, new_tab);
            }
        });
        self.reader.add_controller(click);

        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(self);
        motion.connect_motion(move |_, x, y| {
            if let Some(editor) = weak.upgrade() {
                let target = editor.target_at(x, y);
                editor.reader.set_cursor_from_name(target.as_ref().map(|_| "pointer"));
                editor.hover(target, x, y);
            }
        });
        let weak = Rc::downgrade(self);
        motion.connect_leave(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.hover(None, 0.0, 0.0);
            }
        });
        self.reader.add_controller(motion);

        // Enter continues Markdown lists and quotes.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, state| {
            let Some(editor) = weak.upgrade() else { return glib::Propagation::Proceed };
            let modifiers = gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK;
            // The completion list, while it shows, has Up, Down, Enter, Tab and Esc.
            if editor.completion.borrow().as_ref().is_some_and(|completion| completion.is_open()) && !state.intersects(modifiers) {
                let handled = match key {
                    gdk::Key::Up => editor.step_completion(-1),
                    gdk::Key::Down => editor.step_completion(1),
                    gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::Tab => editor.accept_completion(),
                    gdk::Key::Escape => editor.dismiss_completion(),
                    _ => false,
                };
                if handled {
                    return glib::Propagation::Stop;
                }
            }
            if !matches!(key, gdk::Key::Return | gdk::Key::KP_Enter) || state.intersects(modifiers) {
                return glib::Propagation::Proceed;
            }
            if editor.continue_list() { glib::Propagation::Stop } else { glib::Propagation::Proceed }
        });
        self.view.add_controller(keys);

        // Completion follows the typing and the cursor; it closes when the text loses focus.
        let weak = Rc::downgrade(self);
        self.buffer.connect_mark_set(move |_, _, mark| {
            let Some(editor) = weak.upgrade() else { return };
            match mark.name().as_deref() {
                Some("insert") => {
                    editor.styler.cursor_moved();
                    editor.queue_completion();
                }
                Some("selection_bound") => editor.styler.cursor_moved(),
                _ => {}
            }
        });
        // Live Preview waits while text is composed through an input method, and while a
        // selection is dragged with the mouse (text would move under the pointer).
        let weak = Rc::downgrade(self);
        self.view.connect_preedit_changed(move |_, preedit| {
            if let Some(editor) = weak.upgrade() {
                editor.styler.set_composing(!preedit.is_empty());
            }
        });
        let drag = gtk::GestureClick::builder().button(gdk::BUTTON_PRIMARY).propagation_phase(gtk::PropagationPhase::Capture).build();
        let weak = Rc::downgrade(self);
        drag.connect_pressed(move |_, _, _, _| {
            if let Some(editor) = weak.upgrade() {
                editor.styler.set_dragging(true);
            }
        });
        let weak = Rc::downgrade(self);
        drag.connect_released(move |_, _, _, _| {
            if let Some(editor) = weak.upgrade() {
                editor.styler.set_dragging(false);
            }
        });
        let weak = Rc::downgrade(self);
        drag.connect_cancel(move |_, _| {
            if let Some(editor) = weak.upgrade() {
                editor.styler.set_dragging(false);
            }
        });
        self.view.add_controller(drag);
        // Live Preview: a click on a checkbox ticks it, the cursor staying where it is.
        let tick = gtk::GestureClick::builder().button(gdk::BUTTON_PRIMARY).propagation_phase(gtk::PropagationPhase::Capture).build();
        let weak = Rc::downgrade(self);
        tick.connect_pressed(move |gesture, presses, x, y| {
            let Some(editor) = weak.upgrade() else { return };
            let modifiers = gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK;
            if presses != 1 || gesture.current_event_state().intersects(modifiers) {
                return;
            }
            if let Some(task) = editor.view.task_at(x, y) {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                editor.styler.set_dragging(false);
                editor.tick(task);
            }
        });
        self.view.add_controller(tick);
        // A link opens on a click, as in Obsidian: in Live Preview on a line not being edited,
        // anywhere with Ctrl (in a new tab, as in the rendered view). A #tag searches.
        let follow = gtk::GestureClick::builder().button(gdk::BUTTON_PRIMARY).propagation_phase(gtk::PropagationPhase::Capture).build();
        let weak = Rc::downgrade(self);
        follow.connect_pressed(move |gesture, presses, x, y| {
            let Some(editor) = weak.upgrade() else { return };
            let state = gesture.current_event_state();
            if presses != 1 || state.intersects(gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::ALT_MASK) {
                return;
            }
            let new_tab = state.contains(gdk::ModifierType::CONTROL_MASK);
            let Some(clicked) = editor.clicked_at(x, y, new_tab) else { return };
            gesture.set_state(gtk::EventSequenceState::Claimed);
            editor.styler.set_dragging(false);
            match clicked {
                Clicked::Link(url) => editor.open_link(&url, new_tab),
                Clicked::Tag(tag) => {
                    if let Some(host) = editor.host.upgrade() {
                        host.search_for(&tag);
                    }
                }
            }
        });
        self.view.add_controller(follow);
        let pointer = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(self);
        pointer.connect_motion(move |controller, x, y| {
            if let Some(editor) = weak.upgrade() {
                let ctrl = controller.current_event_state().contains(gdk::ModifierType::CONTROL_MASK);
                let hand = editor.view.task_at(x, y).is_some() || editor.followable_at(x, y, ctrl).is_some();
                editor.view.set_cursor_from_name(Some(if hand { "pointer" } else { "text" }));
            }
        });
        self.view.add_controller(pointer);
        // Live Preview draws pictures found as the rendered view finds them.
        let weak = Rc::downgrade(self);
        self.view.find_with(move |path| weak.upgrade()?.find(path));
        self.styler.set_live(settings::get().live_preview);
        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(self);
        focus.connect_leave(move |_| {
            if let Some(completion) = weak.upgrade().and_then(|editor| editor.completion.borrow().clone()) {
                completion.hide();
            }
        });
        self.view.add_controller(focus);
    }

    /// The file shown, if any.
    pub fn location(&self) -> Option<gio::File> {
        self.file.location()
    }

    pub fn is_modified(&self) -> bool {
        self.buffer.is_modified()
    }

    /// A new, empty note that nothing has happened to yet.
    pub fn is_pristine(&self) -> bool {
        self.location().is_none() && self.restore.borrow().is_none() && !self.is_modified() && self.buffer.char_count() == 0
    }

    pub fn is_reading(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some("read")
    }

    pub fn find_bar(&self) -> Rc<FindBar> {
        self.find
            .borrow_mut()
            .get_or_insert_with(|| {
                let bar = FindBar::new(self.view.upcast_ref(), &self.buffer);
                self.page.prepend(&bar.widget);
                bar
            })
            .clone()
    }

    pub fn set_wrap(&self, on: bool) {
        self.view.set_wrap_mode(if on { gtk::WrapMode::WordChar } else { gtk::WrapMode::None });
    }

    /// Live Preview on or off (Settings → Edit Markdown as).
    pub fn set_live_preview(&self, on: bool) {
        self.styler.set_live(on);
    }

    pub fn set_line_numbers(&self, on: bool) {
        self.view.set_show_line_numbers(on);
        self.fit_width();
    }

    /// Margins for the window's width: the text in a readable column in the middle (Settings →
    /// Text width), or across the window. The line numbers stay beside the text.
    pub fn fit_width(&self) {
        let width = self.view.width();
        if width > 0 {
            const LEAST: i32 = 16;
            let side = side_margin(width, LEAST);
            let gutter = sourceview::prelude::ViewExt::gutter(&self.view, gtk::TextWindowType::Left);
            let numbers = self.view.shows_line_numbers();
            let shift = if numbers {
                let (_, natural, _, _) = gutter.measure(gtk::Orientation::Horizontal, -1);
                (side - LEAST - (natural - gutter.margin_start())).max(0)
            } else {
                0
            };
            if gutter.margin_start() != shift {
                gutter.set_margin_start(shift);
            }
            let left = if numbers { LEAST } else { side };
            if self.view.left_margin() != left {
                self.view.set_left_margin(left);
            }
            if self.view.right_margin() != side {
                self.view.set_right_margin(side);
            }
        }

        let width = self.reader.width();
        if width > 0 {
            let side = side_margin(width, render::MARGIN);
            if self.reader.left_margin() != side {
                self.reader.set_left_margin(side);
                self.reader.set_right_margin(side);
            }
            if side != self.reader_margin.get() {
                self.reader_margin.set(side);
                self.rendered.set(false);
                // Rendered again once the size settles (lists and quotes keep their own margins).
                if let Some(waiting) = self.refit_due.take() {
                    waiting.remove();
                }
                let me = self.me.clone();
                let waiting = glib::timeout_add_local_once(Duration::from_millis(120), move || {
                    if let Some(editor) = me.upgrade() {
                        editor.refit_due.take();
                        if editor.is_reading() && !editor.rendered.get() {
                            editor.rerender();
                        }
                    }
                });
                *self.refit_due.borrow_mut() = Some(waiting);
            }
        }
    }

    /// Enter on a Markdown list item or quote: continues it, or ends it when it is empty.
    /// False when Enter should just do what it always does.
    fn continue_list(&self) -> bool {
        if !settings::get().continue_lists || !self.styler.is_enabled() || self.buffer.has_selection() {
            return false;
        }
        let cursor = self.buffer.iter_at_mark(&self.buffer.get_insert());
        if self.styler.in_code(&cursor) {
            return false;
        }
        let mut start = cursor;
        start.set_line_offset(0);
        let mut end = cursor;
        if !end.ends_line() {
            end.forward_to_line_end();
        }
        let before = self.buffer.text(&start, &cursor, true);
        let after = self.buffer.text(&cursor, &end, true);
        let Some(enter) = lists::on_enter(&before, &after) else { return false };
        self.buffer.begin_user_action();
        match enter {
            lists::Enter::Continue(prefix) => {
                let mut at = cursor;
                self.buffer.insert(&mut at, &format!("\n{prefix}"));
            }
            lists::Enter::Replace(line) => {
                self.buffer.delete(&mut start, &mut end);
                self.buffer.insert(&mut start, &line);
            }
        }
        self.buffer.end_user_action();
        self.view.scroll_mark_onscreen(&self.buffer.get_insert());
        true
    }

    /// Shows what a new note, a link, a search hit or the last session asked for.
    pub fn arrive(self: &Rc<Self>, then: OnLoad) {
        match then {
            OnLoad::Edit => self.set_reading(false),
            OnLoad::Heading(heading) if self.location().is_some_and(|l| is_markdown(&l)) => {
                self.set_reading(true);
                let weak = Rc::downgrade(self);
                after_next_frame(&self.window, move || {
                    if let Some(editor) = weak.upgrade() {
                        editor.jump_to_heading(&heading);
                    }
                });
            }
            OnLoad::Heading(_) => self.reveal_cursor(),
            OnLoad::Hit { line, in_line } => {
                let at = |index: usize| self.buffer.iter_at_line_index(line as i32, index as i32);
                if let (Some(start), Some(end)) = (at(in_line.start), at(in_line.end)) {
                    self.buffer.select_range(&start, &end);
                }
                self.set_reading(false);
            }
            OnLoad::Restore { text, line, column, reading } => {
                if let Some(text) = text {
                    self.buffer.set_text(&text);
                    self.buffer.set_modified(true);
                }
                self.buffer.place_cursor(&self.iter_at(line, column));
                let markdown = self.location().is_none_or(|l| is_markdown(&l));
                if reading && markdown {
                    self.set_reading(true);
                } else {
                    self.set_reading(false);
                }
            }
        }
    }

    /// The open file was renamed: follow it.
    pub fn moved_to(self: &Rc<Self>, location: &gio::File) {
        self.file.set_location(Some(location));
        self.update_title();
        self.watch();
        if let Some(host) = self.host.upgrade() {
            host.select_in_sidebar(location);
        }
        crate::window::store_soon();
    }

    /// Shows `file` here instead of what was shown (which has no unsaved changes).
    pub fn replace_with(self: &Rc<Self>, file: gio::File, then: Option<OnLoad>) {
        self.remember_place();
        *self.note.borrow_mut() = None;
        self.restore.borrow_mut().take();
        self.kept_mine.set(false);
        self.rendered.set(false);
        self.load(file, then);
    }

    /// Focuses whichever view is showing: the rendered one or the source.
    pub fn focus_view(&self) {
        if self.is_reading() {
            self.reader.grab_focus();
        } else {
            self.view.grab_focus();
        }
    }

    /// Shows the rendered Markdown (true) or the source (false).
    pub fn set_reading(&self, on: bool) {
        if on && !self.rendered.get() {
            self.render_markdown();
        }
        if on {
            self.find_backlinks();
        }
        self.stack.set_visible_child_name(if on { "read" } else { "edit" });
        if let Some(host) = self.host.upgrade() {
            host.reading_changed(self, on);
        }
        if on {
            self.reader.grab_focus();
        } else {
            self.view.grab_focus();
            self.reveal_cursor();
        }
    }

    /// Scrolls the source view to the cursor once it has been laid out.
    fn reveal_cursor(&self) {
        let (view, buffer) = (self.view.clone(), self.buffer.clone());
        after_next_frame(&self.window, move || {
            view.scroll_to_mark(&buffer.get_insert(), 0.1, true, 0.0, 0.3);
        });
    }

    /// The iter at `line` and `column`, or as close as the text allows.
    fn iter_at(&self, line: u32, column: u32) -> gtk::TextIter {
        let (line, column) = (line as i32, column as i32);
        if let Some(iter) = self.buffer.iter_at_line_offset(line, column) {
            return iter;
        }
        match self.buffer.iter_at_line(line) {
            Some(mut iter) => {
                iter.forward_to_line_end();
                iter
            }
            None => self.buffer.end_iter(),
        }
    }

    /// Remembers where the cursor is in this file.
    pub fn remember_place(&self) {
        if self.restore.borrow().is_some() {
            return;
        }
        if let Some(key) = self.location().map(|l| l.parse_name().to_string()) {
            let cursor = self.buffer.iter_at_mark(&self.buffer.get_insert());
            positions::keep(&key, cursor.line() as u32, cursor.line_offset() as u32);
        }
    }

    fn render_markdown(&self) {
        // Margins for the width the view has, or will have: a tab shown for the first time
        // isn't laid out yet, and centred text shouldn't move once it is.
        let width = [self.reader.width(), self.page.width(), self.window.width()].into_iter().find(|&w| w > 0);
        if let Some(width) = width {
            let side = side_margin(width, render::MARGIN);
            self.reader_margin.set(side);
            self.reader.set_left_margin(side);
            self.reader.set_right_margin(side);
        }
        let text = self.text();
        // Links by an alias are found once the vault's notes are read (and notes linking here
        // by one of its aliases): rendered and searched again then.
        if let Some((root, _, _)) = self.vault() {
            let me = self.me.clone();
            let _ = vault::index(&root, move |_| {
                if let Some(editor) = me.upgrade() {
                    editor.refresh(false);
                    if editor.is_reading() {
                        editor.find_backlinks();
                    }
                }
            });
        }
        let folder = self.location().and_then(|l| l.parent());
        let reader = self.reader.clone();
        let layout = move |text: &str| reader.create_pango_layout(Some(text));
        let find = |path: &str| self.find(path);
        let rendered =
            render::render(&text, folder.as_ref(), &find, theme::scheme().as_ref(), &layout, self.reader_margin.get());
        self.reader.show_buffer(&rendered.buffer);
        self.reader.set_panels(rendered.panels);
        for (anchor, widget) in &rendered.children {
            self.reader.add_child_at_anchor(widget, anchor);
        }
        *self.targets.borrow_mut() = rendered.targets;
        *self.headings.borrow_mut() = rendered.headings;
        *self.lines.borrow_mut() = rendered.lines;
        self.rendered.set(true);
        self.show_backlinks();
        devhooks::lap("markdown rendered");
        if !rendered.code.is_empty() {
            let (buffer, code) = (rendered.buffer, rendered.code);
            after_next_frame(&self.window, move || {
                render::highlight_code(&buffer, &code, theme::scheme().as_ref());
                devhooks::lap("code blocks coloured");
            });
        }
    }

    /// Looks for the notes in the folder that link to this one, in the background; they show
    /// under the rendered note. Only in a folder window.
    fn find_backlinks(&self) {
        let (Some(host), Some(note)) = (self.host.upgrade(), self.location()) else { return };
        if host.folder().is_none() {
            return;
        }
        // In the note's vault (the folder holding .obsidian, or the window's folder).
        let Some((on_disk, files, _)) = self.vault() else { return };
        let Some(to) = note.path().and_then(|path| path.strip_prefix(&on_disk).ok().map(|to| to.to_path_buf())) else { return };
        let to = to.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
        let root = gio::File::for_path(&on_disk);
        let files: Vec<String> = files.as_ref().clone();
        let aliases = vault::aliases_of(&on_disk, &to);
        let search = self.backlink_searches.get() + 1;
        self.backlink_searches.set(search);
        let me = self.me.clone();
        glib::spawn_future_local(async move {
            let Ok(found) = gio::spawn_blocking(move || backlinks::find(&on_disk, &to, &files, &aliases)).await else { return };
            let Some(editor) = me.upgrade() else { return };
            if editor.backlink_searches.get() != search {
                return;
            }
            *editor.backlinks.borrow_mut() = Some(Backlinks { root, note, found });
            if editor.rendered.get() {
                editor.show_backlinks();
            }
        });
    }

    /// Under the rendered note, the notes found linking to it (replacing those shown before).
    fn show_backlinks(&self) {
        let cache = self.backlinks.borrow();
        let Some(Backlinks { root, note, found }) = cache.as_ref() else { return };
        if !self.location().is_some_and(|location| location.equal(note)) {
            return;
        }
        let buffer = self.reader.buffer();
        if let Some(mark) = buffer.mark("backlinks") {
            buffer.delete(&mut buffer.iter_at_mark(&mark), &mut buffer.end_iter());
        } else {
            buffer.create_mark(Some("backlinks"), &buffer.end_iter(), true);
        }
        if found.is_empty() {
            return;
        }
        let colors = theme::colors();
        let margin = render::MARGIN;
        let table = buffer.tag_table();
        let add = |tag: gtk::TextTag| {
            table.add(&tag);
            tag
        };
        // A blank line, then a hairline: a line's background would cover the space above it.
        let space = add(gtk::TextTag::builder().pixels_above_lines(16).build());
        let rule = add(gtk::TextTag::builder().size_points(1.0).paragraph_background(colors.rule.to_hex()).build());
        let title = add(gtk::TextTag::builder()
            .foreground(colors.dim.to_hex())
            .weight(600)
            .scale(0.9)
            .left_margin(margin)
            .pixels_above_lines(14)
            .pixels_below_lines(6)
            .build());
        let line = add(gtk::TextTag::builder()
            .foreground(colors.dim.to_hex())
            .scale(0.9)
            .left_margin(margin + 18)
            .right_margin(margin)
            .pixels_below_lines(2)
            .build());
        // Right-to-left lines (Persian) sit at the left too, under their note: for right-to-left
        // text, GTK's "right" is where its lines end.
        let rtl = add(gtk::TextTag::builder().justification(gtk::Justification::Right).build());
        let place = |text: &str, tag: &gtk::TextTag| -> Vec<gtk::TextTag> {
            match gtk::pango::find_base_dir(text) {
                gtk::pango::Direction::Rtl => vec![tag.clone(), rtl.clone()],
                _ => vec![tag.clone()],
            }
        };
        let mut end = buffer.end_iter();
        buffer.insert_with_tags(&mut end, "\n", &[&space]);
        buffer.insert_with_tags(&mut end, " \n", &[&rule]);
        let count = found.len();
        let heading = if count == 1 { "Linked from 1 note\n".to_owned() } else { format!("Linked from {count} notes\n") };
        buffer.insert_with_tags(&mut end, &heading, &[&title]);
        let mut targets = self.targets.borrow_mut();
        for (path, mentions) in found {
            let name = add(gtk::TextTag::builder()
                .foreground(colors.link.to_hex())
                .underline(gtk::pango::Underline::Single)
                .left_margin(margin)
                .pixels_above_lines(6)
                .build());
            targets.push((name.clone(), Target::File(root.resolve_relative_path(path))));
            let shown = path.rsplit_once('.').map_or(path.as_str(), |(stem, _)| stem);
            let tags = place(shown, &name);
            buffer.insert_with_tags(&mut end, &format!("{shown}\n"), &tags.iter().collect::<Vec<_>>());
            for mention in mentions.iter().take(3) {
                let text: String = mention.text.chars().take(160).collect();
                let tags = place(&text, &line);
                buffer.insert_with_tags(&mut end, &format!("{text}\n"), &tags.iter().collect::<Vec<_>>());
            }
        }
    }

    /// Re-renders the rendered view in place, keeping its scroll position.
    fn rerender(&self) {
        let scroll = self.reader.vadjustment().map(|a| a.value());
        self.render_markdown();
        if let (Some(value), Some(adjustment)) = (scroll, self.reader.vadjustment()) {
            after_next_frame(&self.window, move || adjustment.set_value(value));
        }
    }

    /// The font or the colours changed: renders again what shows rendered.
    pub fn refresh(&self, colors_too: bool) {
        if colors_too {
            self.buffer.set_style_scheme(theme::scheme().as_ref());
            self.styler.recolor();
        }
        self.rendered.set(false);
        if self.is_reading() {
            self.rerender();
        }
    }

    /// The note's headings, for the outline; other files have none.
    pub fn outline(&self) -> Vec<outline::Heading> {
        if self.styler.is_enabled() { outline::headings(&self.text()) } else { Vec::new() }
    }

    /// Which of `headings` the reader is in: the last one above the cursor, or above the top
    /// of the rendered view.
    pub fn current_heading(&self, headings: &[outline::Heading]) -> Option<usize> {
        if self.is_reading() {
            let (top, _) = self.reader.line_at_y(self.reader.visible_rect().y());
            let buffer = self.reader.buffer();
            let marks = self.headings.borrow();
            return marks.iter().rposition(|(_, mark)| buffer.iter_at_mark(mark).offset() <= top.offset());
        }
        let line = self.buffer.iter_at_mark(&self.buffer.get_insert()).line() as usize;
        headings.iter().rposition(|heading| heading.line <= line)
    }

    /// Shows the `index`th of the note's `headings`: scrolled to in the rendered view, or with
    /// the cursor on its line in the source.
    pub fn go_to_heading(&self, index: usize, headings: &[outline::Heading]) {
        if self.is_reading() {
            if let Some((_, mark)) = self.headings.borrow().get(index) {
                self.reader.scroll_to_mark(mark, 0.0, true, 0.0, 0.0);
            }
        } else if let Some(iter) = headings.get(index).and_then(|heading| self.buffer.iter_at_line(heading.line as i32)) {
            self.buffer.place_cursor(&iter);
            self.view.scroll_to_mark(&self.buffer.get_insert(), 0.0, true, 0.0, 0.1);
        }
    }

    /// The vault this note is in (or would be, for one not saved yet: the window's folder), its
    /// files, and the note's folder within it.
    fn vault(&self) -> Option<(std::path::PathBuf, Rc<Vec<String>>, String)> {
        let folder = self.location().and_then(|location| location.parent()).and_then(|folder| folder.path());
        self.vault_near(folder)
    }

    /// The vault a note in `folder` is in (the window's folder for none), its files, and the
    /// folder within it.
    fn vault_near(&self, folder: Option<std::path::PathBuf>) -> Option<(std::path::PathBuf, Rc<Vec<String>>, String)> {
        let window = self.host.upgrade().and_then(|host| host.folder()).and_then(|folder| folder.path());
        let folder = folder.or(window.clone())?;
        let root = vault::root_of(&folder, window.as_deref());
        let from = folder.strip_prefix(&root).ok()?.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
        let files = vault::files(&root);
        Some((root, files, from))
    }

    /// The file a link in this note names (its path, decoded), the way Obsidian finds it.
    fn find(&self, path: &str) -> Option<gio::File> {
        let (root, files, from) = self.vault()?;
        // By a file's name, or else by a note's aliases.
        let found = vault::resolve(path, &from, &files).or_else(|| vault::by_alias(path, &vault::index(&root, |_| {})?))?;
        Some(gio::File::for_path(root.join(found)))
    }

    /// The pointer is on `target` (or nothing) in the rendered view, at `x`, `y`: resting on a
    /// link to a note, the note's start shows in a popover after a moment.
    fn hover(&self, target: Option<Target>, x: f64, y: f64) {
        let file = match target {
            Some(Target::Link(url)) if !links::has_scheme(&url) => {
                let path = links::split(&url).path;
                (!path.is_empty()).then(|| self.find(&path)).flatten()
            }
            Some(Target::File(file)) => Some(file),
            _ => None,
        }
        .filter(is_text);
        let same = matches!((self.previewing.borrow().as_ref(), file.as_ref()), (Some(a), Some(b)) if a.equal(b));
        if same {
            return;
        }
        if let Some(waiting) = self.preview_due.take() {
            waiting.remove();
        }
        if let Some(popover) = self.preview.borrow().as_ref() {
            popover.popdown();
        }
        *self.previewing.borrow_mut() = file.clone();
        let Some(file) = file else { return };
        let me = self.me.clone();
        let waiting = glib::timeout_add_local_once(Duration::from_millis(450), move || {
            if let Some(editor) = me.upgrade() {
                editor.preview_due.take();
                editor.show_preview(&file, x, y);
            }
        });
        *self.preview_due.borrow_mut() = Some(waiting);
    }

    /// The start of the note in `file`, rendered in a popover at `x`, `y`.
    fn show_preview(&self, file: &gio::File, x: f64, y: f64) {
        let Ok((bytes, _)) = file.load_contents(None::<&gio::Cancellable>) else { return };
        let text = String::from_utf8_lossy(&bytes);
        // Enough to know the note by, not the whole of a long one.
        let start = text.lines().take(40).collect::<Vec<_>>().join("\n");
        let view = crate::paper::Paper::new();
        view.set_editable(false);
        view.set_cursor_visible(false);
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_top_margin(10);
        view.set_bottom_margin(10);
        view.add_css_class("nevis-preview");
        let folder = file.parent();
        let near = folder.as_ref().and_then(|folder| folder.path());
        let find = |path: &str| {
            let (root, files, from) = self.vault_near(near.clone())?;
            vault::resolve(path, &from, &files).map(|found| gio::File::for_path(root.join(found)))
        };
        let layout = |text: &str| view.create_pango_layout(Some(text));
        let rendered = render::render(&start, folder.as_ref(), &find, theme::scheme().as_ref(), &layout, 14);
        view.set_buffer(Some(&rendered.buffer));
        view.set_panels(rendered.panels);
        view.set_left_margin(14);
        view.set_right_margin(14);
        for (anchor, widget) in &rendered.children {
            view.add_child_at_anchor(widget, anchor);
        }
        let scroller = gtk::ScrolledWindow::builder()
            .child(&view)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(320)
            .width_request(460)
            .build();
        let mut kept = self.preview.borrow_mut();
        let popover = kept.get_or_insert_with(|| {
            let popover = gtk::Popover::builder().autohide(false).can_focus(false).build();
            popover.add_css_class("nevis-preview-popover");
            popover.set_parent(&self.reader);
            popover
        });
        popover.set_child(Some(&scroller));
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.popup();
    }

    /// Scrolls the rendered view to the heading a `#…` link names.
    /// Goes to the heading `target` names (or the block, for `^id`): in either view.
    fn jump_to_heading(&self, target: &str) {
        if let Some(id) = target.strip_prefix('^') {
            let text = self.text();
            let Some(block) = obsidian::block(&text, id) else { return };
            let line = text[..block.start].matches('\n').count();
            if self.is_reading() {
                let lines = self.lines.borrow();
                let at = lines.partition_point(|(start, _)| *start <= line);
                if let Some((_, mark)) = at.checked_sub(1).and_then(|i| lines.get(i)) {
                    self.reader.scroll_to_mark(mark, 0.0, true, 0.0, 0.0);
                }
            } else if let Some(iter) = self.buffer.iter_at_line(line as i32) {
                self.buffer.place_cursor(&iter);
                self.view.scroll_to_mark(&self.buffer.get_insert(), 0.0, true, 0.0, 0.1);
            }
            return;
        }
        let headings = outline::headings(&self.text());
        if let Some(index) = headings.iter().position(|heading| anchors::points_at(target, &heading.text)) {
            self.go_to_heading(index, &headings);
        }
    }

    fn target_at(&self, x: f64, y: f64) -> Option<Target> {
        let (bx, by) = self.reader.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        let iter = self.reader.iter_at(bx, by)?;
        let targets = self.targets.borrow();
        targets.iter().find(|(tag, _)| iter.has_tag(tag)).map(|(_, target)| target.clone())
    }

    fn follow(self: &Rc<Self>, target: Target, new_tab: bool) {
        match target {
            Target::Task(range) => self.toggle_task(range),
            Target::Link(url) => self.open_link(&url, new_tab),
            Target::File(file) => {
                if let Some(host) = self.host.upgrade() {
                    host.open(file, None, if new_tab { How::NewTab } else { How::Replace });
                }
            }
            Target::Tag(tag) => {
                if let Some(host) = self.host.upgrade() {
                    host.search_for(&tag);
                }
            }
            Target::Fold(fold) => {
                let opening = fold.is_invisible();
                fold.set_invisible(!opening);
                // The header's last character says which: "−" open, "+" closed.
                let header = self.targets.borrow().iter().find(|(_, target)| matches!(target, Target::Fold(f) if *f == fold)).map(|(tag, _)| tag.clone());
                if let Some(header) = header {
                    let buffer = self.reader.buffer();
                    let mut end = buffer.start_iter();
                    if (end.starts_tag(Some(&header)) || end.forward_to_tag_toggle(Some(&header))) && end.forward_to_tag_toggle(Some(&header)) {
                        let mut sign = end;
                        sign.backward_char();
                        buffer.delete(&mut sign, &mut end);
                        buffer.insert_with_tags(&mut sign, if opening { "\u{2212}" } else { "+" }, &[&header]);
                    }
                }
            }
        }
    }

    /// The link or tag at `x`, `y` in the source view a click follows (see `followable_at`): the
    /// link's address, read from the text, or the tag.
    fn clicked_at(&self, x: f64, y: f64, ctrl: bool) -> Option<Clicked> {
        let (offset, tag) = self.followable_at(x, y, ctrl)?;
        let text = self.text();
        if tag {
            let span = self.styler.span_at(&[style::Kind::Tag], offset)?;
            return Some(Clicked::Tag(text.chars().skip(span.start).take(span.len()).collect()));
        }
        let byte = text.char_indices().nth(offset)?.0;
        links::at(&text, byte).map(Clicked::Link)
    }

    /// The character at `x`, `y` in the source view if a click there follows a link (or searches
    /// a tag: then true): in Live Preview on a line not being edited, or with `ctrl`.
    fn followable_at(&self, x: f64, y: f64, ctrl: bool) -> Option<(usize, bool)> {
        let (bx, by) = self.view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        let iter = self.view.iter_at(bx, by)?;
        // Past a line's end isn't on it.
        if iter.ends_line() || !(ctrl || (self.styler.is_live() && !self.styler.is_editing(iter.line() as usize))) {
            return None;
        }
        let offset = iter.offset() as usize;
        if self.styler.span_at(&[style::Kind::LinkText, style::Kind::Url], offset).is_some() {
            return Some((offset, false));
        }
        self.styler.span_at(&[style::Kind::Tag], offset).map(|_| (offset, true))
    }

    /// Ticks the task whose "[ ]" is at `task` (characters), or unticks it, as one undo step.
    fn tick(&self, task: Range<usize>) {
        let mut start = self.buffer.iter_at_offset(task.start as i32 + 1);
        let mut end = start;
        end.forward_char();
        let ticked = start.char() != ' ';
        self.buffer.begin_user_action();
        self.buffer.delete(&mut start, &mut end);
        self.buffer.insert(&mut start, if ticked { " " } else { "x" });
        self.buffer.end_user_action();
    }

    /// Flips a `[ ]` / `[x]` checkbox in the source and re-renders in place.
    fn toggle_task(&self, marker: Range<usize>) {
        let text = self.text();
        let Some(open) = text.get(marker.clone()).and_then(|m| m.find('[')) else { return };
        let at = marker.start + open + 1;
        let Some(mark) = text[at..].chars().next() else { return };
        let offset = text[..at].chars().count() as i32;
        let mut start = self.buffer.iter_at_offset(offset);
        let mut end = self.buffer.iter_at_offset(offset + 1);
        self.buffer.begin_user_action();
        self.buffer.delete(&mut start, &mut end);
        self.buffer.insert(&mut start, if mark == ' ' { "x" } else { " " });
        self.buffer.end_user_action();

        // Re-rendering replaces the buffer, which scrolls to the top; rerender puts it back.
        self.rerender();
    }

    /// Follows a link: web addresses in the browser, notes here (in a new tab if asked).
    pub fn open_link(self: &Rc<Self>, url: &str, new_tab: bool) {
        if links::has_scheme(url) {
            return launch(&self.window, url);
        }
        // A path relative to this note or a [[wiki]] name, maybe with a #heading.
        let target = links::split(url);
        if target.path.is_empty() {
            if let Some(heading) = target.heading {
                self.jump_to_heading(&heading);
            }
            return;
        }
        let Some(host) = self.host.upgrade() else { return };
        // As Obsidian finds it: beside this note, else anywhere in the vault by its name.
        // A note that doesn't exist yet is made where the link says, beside this one.
        let base = self.location().and_then(|l| l.parent()).or_else(|| host.folder());
        let file = self.find(&target.path).unwrap_or_else(|| match base {
            Some(folder) => folder.resolve_relative_path(&target.path),
            None => gio::File::for_path(&target.path),
        });
        if !is_text(&file) {
            return launch(&self.window, &file.uri());
        }
        let how = if new_tab { How::NewTab } else { How::Replace };
        host.open(file, target.heading.map(OnLoad::Heading), how);
    }

    pub fn load(self: &Rc<Self>, location: gio::File, then: Option<OnLoad>) {
        // Styled once its kind is known, after the text first shows (set_language).
        self.styler.set_enabled(false);
        *self.on_load.borrow_mut() = then;
        self.file.set_location(Some(&location));
        self.update_title();
        self.read_file(false);
    }

    /// Loads what the last session left in this tab, the first time it is shown.
    pub fn ensure_loaded(self: &Rc<Self>) {
        let Some(state) = self.restore.borrow_mut().take() else { return };
        let text = state.draft.as_deref().and_then(session::read_draft);
        if text.is_some() {
            *self.draft.borrow_mut() = state.draft.clone();
        }
        let then = OnLoad::Restore { text, line: state.line, column: state.column, reading: state.reading };
        match state.path {
            Some(path) => self.load(gio::File::for_path(path), Some(then)),
            None => {
                self.arrive(then);
                self.update_tab();
                devhooks::content_ready(&self.window);
            }
        }
    }

    /// Reads the file into the buffer. A reload keeps the cursor, the scroll positions and
    /// whichever view is showing.
    fn read_file(self: &Rc<Self>, reload: bool) {
        let restore = reload.then(|| Reload {
            cursor: self.buffer.cursor_position(),
            edit_scroll: self.view.vadjustment().map(|a| a.value()),
            read_scroll: self.reader.vadjustment().map(|a| a.value()),
        });
        self.view.set_editable(false);
        let loader = sourceview::FileLoader::new(&self.buffer, &self.file);
        let weak = Rc::downgrade(self);
        spawn(async move {
            let (load, _progress) = loader.load_future(glib::Priority::DEFAULT);
            let result = load.await;
            devhooks::lap("file loaded");
            if let Some(editor) = weak.upgrade() {
                match restore {
                    Some(restore) => editor.reloaded(result, restore),
                    None => editor.loaded(result),
                }
            }
        });
    }

    fn reloaded(self: &Rc<Self>, result: Result<(), glib::Error>, restore: Reload) {
        self.view.set_editable(true);
        if let Err(err) = result {
            alert(&self.window, "Could not reload the file", err.message());
            return;
        }
        self.kept_mine.set(false);
        *self.note.borrow_mut() = None;
        let cursor = restore.cursor.min(self.buffer.char_count());
        self.buffer.place_cursor(&self.buffer.iter_at_offset(cursor));
        self.buffer.set_modified(false);
        self.update_title();
        if self.is_reading() {
            self.render_markdown();
        }
        // Replacing the text scrolls to the top; put both views back where they were.
        let (view, reader) = (self.view.clone(), self.reader.clone());
        after_next_frame(&self.window, move || {
            if let (Some(value), Some(adjustment)) = (restore.edit_scroll, view.vadjustment()) {
                adjustment.set_value(value);
            }
            if let (Some(value), Some(adjustment)) = (restore.read_scroll, reader.vadjustment()) {
                adjustment.set_value(value);
            }
        });
    }

    /// Watches the file on disk for changes made by other programs.
    fn watch(self: &Rc<Self>) {
        let monitor = self
            .location()
            .and_then(|l| l.monitor_file(gio::FileMonitorFlags::WATCH_MOVES, None::<&gio::Cancellable>).ok());
        if let Some(monitor) = &monitor {
            let weak = Rc::downgrade(self);
            monitor.connect_changed(move |_, _, _, event| {
                if event != gio::FileMonitorEvent::AttributeChanged
                    && let Some(editor) = weak.upgrade()
                {
                    editor.schedule_disk_check();
                }
            });
        }
        *self.monitor.borrow_mut() = monitor;
    }

    fn schedule_disk_check(self: &Rc<Self>) {
        if self.checking.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        // Programs often write a file in several steps; look once they are done.
        glib::timeout_add_local_once(Duration::from_millis(200), move || {
            if let Some(editor) = weak.upgrade() {
                editor.checking.set(false);
                editor.check_disk();
            }
        });
    }

    pub fn check_disk(self: &Rc<Self>) {
        // Our own save, a question already on screen, or not loaded yet.
        if self.saving.get() || self.asking.get() || self.restore.borrow().is_some() {
            return;
        }
        self.file.check_file_on_disk();
        if self.file.is_deleted() {
            *self.note.borrow_mut() = Some("deleted on disk".to_owned());
            self.update_title();
        } else if self.file.is_externally_modified() {
            if !self.is_modified() {
                self.read_file(true);
            } else if !self.kept_mine.get() {
                spawn(self.clone().ask_reload());
            }
        }
    }

    async fn ask_reload(self: Rc<Self>) {
        self.asking.set(true);
        let message = format!("“{}” changed on disk", self.display_name());
        let detail = "Reload it and lose your unsaved changes here, or keep your version?";
        let choice = choose(&self.window, &message, detail, &["Keep Mine", "Reload"]).await;
        self.asking.set(false);
        if choice == Some(1) {
            self.read_file(true);
        } else {
            self.kept_mine.set(true);
        }
    }

    fn loaded(self: &Rc<Self>, result: Result<(), glib::Error>) {
        let mut editable = true;
        let note = match result {
            Ok(()) => None,
            Err(err) if err.matches(gio::IOErrorEnum::NotFound) => Some("new file".to_owned()),
            Err(err) if err.matches(sourceview::FileLoaderError::ConversionFallback) => {
                // Not text in any encoding we know; editing and saving would corrupt it.
                editable = false;
                Some("read-only: not valid text".to_owned())
            }
            Err(err) => {
                self.file.set_location(None::<&gio::File>);
                alert(&self.window, "Could not open the file", err.message());
                None
            }
        };
        let existing = note.is_none();
        *self.note.borrow_mut() = note;
        self.watch();
        if let (Some(host), Some(location)) = (self.host.upgrade(), self.location()) {
            host.select_in_sidebar(&location);
        }
        self.view.set_editable(editable);
        let place = self.location().and_then(|l| positions::recall(&l.parse_name()));
        let cursor = place.map_or_else(|| self.buffer.start_iter(), |(line, column)| self.iter_at(line, column));
        self.buffer.place_cursor(&cursor);
        self.buffer.set_modified(false);
        self.update_title();
        let markdown = existing && self.location().is_some_and(|l| is_markdown(&l));
        // An empty note has nothing to read: it opens ready for typing.
        let empty = self.buffer.char_count() == 0;
        match self.on_load.take() {
            Some(then) => self.arrive(then),
            None if markdown && !empty && settings::get().open_rendered => self.set_reading(true),
            None => self.reveal_cursor(),
        }
        devhooks::content_ready(&self.window);

        // Loading the highlighting rules takes ~100 ms; show the text first.
        let weak = Rc::downgrade(self);
        after_next_frame(&self.window, move || {
            if let Some(editor) = weak.upgrade()
                && let Some(location) = editor.location()
            {
                editor.set_language(&location);
                devhooks::lap("highlighting applied");
            }
        });
    }

    fn set_language(&self, location: &gio::File) {
        // Markdown gets nevis's own styling (style.rs) rather than GtkSourceView's highlighting.
        let markdown = is_markdown(location);
        self.styler.set_enabled(markdown);
        let language = if markdown || extension(location).is_some_and(|e| e == "txt" || e == "text") {
            None
        } else {
            // By its name, or by how the file starts ("#!/bin/sh", "<?xml"…) when that says more.
            let start = self.buffer.start_iter();
            let mut end = start;
            end.forward_chars(1024);
            let head = self.buffer.text(&start, &end, true);
            let name = location.basename();
            let (kind, _) = gio::content_type_guess(name.as_deref(), head.as_bytes());
            sourceview::LanguageManager::default().guess_language(name.as_deref(), Some(&kind))
        };
        self.buffer.set_language(language.as_ref());
    }

    /// Saves to the current file, asking for a name first if there is none. True if saved.
    pub async fn save(self: Rc<Self>) -> bool {
        if self.location().is_none() {
            return self.save_as().await;
        }
        self.write().await
    }

    pub async fn save_as(self: Rc<Self>) -> bool {
        let untitled = self.location().is_none();
        let dialog = gtk::FileDialog::builder().title("Save As").modal(true).build();
        match self.location() {
            Some(location) => dialog.set_initial_file(Some(&location)),
            None => {
                dialog.set_initial_name(Some(&format!("{}.md", self.untitled_name().replace('/', "-"))));
                if let Some(folder) = self.host.upgrade().and_then(|host| host.folder()) {
                    dialog.set_initial_folder(Some(&folder));
                }
            }
        }
        let Ok(location) = dialog.save_future(Some(&self.window)).await else { return false };
        if untitled {
            self.settle_attachments(&location);
        }
        self.file.set_location(Some(&location));
        self.set_language(&location);
        *self.note.borrow_mut() = None;
        self.update_title();
        let saved = self.write().await;
        self.watch();
        crate::window::store_soon();
        saved
    }

    /// Prints the note (a Markdown note as the rendered view shows it), or with `pdf` writes it
    /// to that file as a PDF.
    pub async fn print(self: Rc<Self>, pdf: Option<gio::File>) {
        let text = self.text();
        let find = |path: &str| self.find(path);
        // By its name too: a note just opened is styled as Markdown only once it is shown.
        let content = if self.styler.is_enabled() || self.location().is_some_and(|l| is_markdown(&l)) {
            print::Content::Markdown { text: &text, folder: self.location().and_then(|l| l.parent()), find: &find }
        } else {
            print::Content::Plain { text: &text, language: self.buffer.language() }
        };
        if let Err(error) = print::print(&self.window, &self.layer, &self.print_name(), content, pdf).await {
            alert(&self.window, "Couldn't print", &error);
        }
    }

    /// Asks where, then writes the note there as a PDF (see `print`).
    pub async fn export_pdf(self: Rc<Self>) {
        let name = self.print_name();
        let stem = std::path::Path::new(&name).file_stem().map_or(name.clone(), |stem| stem.to_string_lossy().into_owned());
        let dialog = gtk::FileDialog::builder().title("Export as PDF").modal(true).initial_name(format!("{stem}.pdf")).build();
        if let Some(folder) = self.location().and_then(|l| l.parent()) {
            dialog.set_initial_folder(Some(&folder));
        }
        let Ok(file) = dialog.save_future(Some(&self.window)).await else { return };
        self.print(Some(file)).await;
    }

    /// The file's name, or what an unsaved note would be named.
    fn print_name(&self) -> String {
        self.location()
            .and_then(|l| l.basename())
            .map_or_else(|| self.untitled_name().replace('/', "-"), |name| name.to_string_lossy().into_owned())
    }

    /// The selected words become a link to `address`: [words](address).
    fn link_selection(&self, address: &str) {
        let Some((mut start, mut end)) = self.buffer.selection_bounds() else { return };
        let words = self.buffer.text(&start, &end, true);
        self.buffer.begin_user_action();
        self.buffer.delete(&mut start, &mut end);
        self.buffer.insert(&mut start, &format!("[{words}]({address})"));
        self.buffer.end_user_action();
    }

    /// Ctrl+K in a Markdown note: the selected words become a link's text, with the cursor
    /// where its address goes; a selected address becomes its address, and with nothing
    /// selected an empty link waits for its text.
    pub fn make_link(&self) {
        if !self.styler.is_enabled() || self.is_reading() {
            return;
        }
        self.buffer.begin_user_action();
        let (words, address) = match self.buffer.selection_bounds() {
            Some((mut start, mut end)) => {
                let selected = self.buffer.text(&start, &end, true).to_string();
                self.buffer.delete(&mut start, &mut end);
                match selected.trim() {
                    address if links::is_web_address(address) => (String::new(), address.to_owned()),
                    _ => (selected, String::new()),
                }
            }
            None => (String::new(), String::new()),
        };
        let at = self.buffer.iter_at_mark(&self.buffer.get_insert()).offset();
        self.buffer.insert_at_cursor(&format!("[{words}]({address})"));
        // Inside the brackets when the text is missing, else inside the parentheses.
        let inside = if words.is_empty() { 1 } else { words.chars().count() as i32 + 3 };
        self.buffer.place_cursor(&self.buffer.iter_at_offset(at + inside));
        self.buffer.end_user_action();
    }

    /// Replaces the text as one change to undo, the cursor kept about where it was.
    pub fn replace_text(&self, text: &str) {
        let at = self.buffer.iter_at_mark(&self.buffer.get_insert()).offset();
        self.buffer.begin_user_action();
        self.buffer.set_text(text);
        self.buffer.end_user_action();
        self.buffer.place_cursor(&self.buffer.iter_at_offset(at));
    }

    /// Saves a pasted image as a file and links it where the cursor is: in `attachments/`
    /// beside the note, or aside until the note has a file.
    fn paste_image(&self, texture: &gdk::Texture) {
        let Ok(now) = glib::DateTime::now_local() else { return };
        let when = (now.year(), now.month() as u32, now.day_of_month() as u32, now.hour() as u32, now.minute() as u32, now.second() as u32);
        let beside = self.location().and_then(|location| location.parent()).map(|folder| folder.child("attachments"));
        let folder = beside.clone().unwrap_or_else(|| gio::File::for_path(attachments::aside()));
        let none = None::<&gio::Cancellable>;
        let _ = folder.make_directory_with_parents(none);
        let name = attachments::name(when, |name| folder.child(name).query_exists(none));
        let Some(path) = folder.child(&name).path() else { return };
        if let Err(error) = texture.save_to_png(&path) {
            return alert(&self.window, "Couldn't save the pasted image", &error.to_string());
        }
        vault::forget();
        let link = match beside {
            Some(_) => attachments::link(&format!("attachments/{name}")),
            None => attachments::link(&path.to_string_lossy()),
        };
        // On a line of its own, like any picture in a note.
        self.buffer.begin_user_action();
        self.buffer.delete_selection(true, true);
        let cursor = self.buffer.iter_at_mark(&self.buffer.get_insert());
        let before = if cursor.starts_line() { "" } else { "\n" };
        let after = if cursor.ends_line() { "" } else { "\n" };
        self.buffer.insert_at_cursor(&format!("{before}{link}{after}"));
        self.buffer.end_user_action();
    }

    /// A note saved for the first time: the images pasted into it while it had no file move
    /// beside it, into `attachments/`, and its links to them follow.
    fn settle_attachments(&self, location: &gio::File) {
        let aside = attachments::aside();
        let Some(aside_text) = aside.to_str() else { return };
        let (_, moved) = attachments::settle(&self.text(), aside_text);
        let Some(folder) = location.parent().map(|folder| folder.child("attachments")) else { return };
        if moved.is_empty() {
            return;
        }
        let none = None::<&gio::Cancellable>;
        let _ = folder.make_directory_with_parents(none);
        for name in &moved {
            let _ = gio::File::for_path(aside.join(name)).move_(&folder.child(name), gio::FileCopyFlags::NONE, none, None);
        }
        // Each link in place, as one change to undo.
        let (start, settled) = attachments::starts(aside_text);
        self.buffer.begin_user_action();
        let mut from = self.buffer.start_iter();
        while let Some((mut found, mut end)) = from.forward_search(&start, gtk::TextSearchFlags::TEXT_ONLY, None) {
            self.buffer.delete(&mut found, &mut end);
            self.buffer.insert(&mut found, settled);
            from = found;
        }
        self.buffer.end_user_action();
    }

    async fn write(&self) -> bool {
        if !self.view.is_editable() || self.saving.replace(true) {
            return false;
        }
        let saver = sourceview::FileSaver::new(&self.buffer, &self.file);
        // A saver may be reconfigured and rerun after an error.
        let saved = loop {
            let (save, _progress) = saver.save_future(glib::Priority::DEFAULT);
            match save.await {
                Ok(()) => break true,
                Err(err) if err.matches(sourceview::FileSaverError::ExternallyModified) => {
                    let detail = "Saving will overwrite the changes made outside nevis.";
                    if choose(&self.window, "The file changed on disk", detail, &["Cancel", "Overwrite"]).await != Some(1) {
                        break false;
                    }
                    saver.set_flags(saver.flags() | sourceview::FileSaverFlags::IGNORE_MODIFICATION_TIME);
                }
                Err(err) if err.matches(sourceview::FileSaverError::InvalidChars) => {
                    let detail = "Some characters can't be stored in this file's encoding.";
                    if choose(&self.window, "Save as UTF-8?", detail, &["Cancel", "Save as UTF-8"]).await != Some(1) {
                        break false;
                    }
                    saver.set_encoding(Some(&sourceview::Encoding::utf8()));
                }
                Err(err) => {
                    alert(&self.window, "Could not save the file", err.message());
                    break false;
                }
            }
        };
        self.saving.set(false);
        if saved {
            self.kept_mine.set(false);
            self.buffer.set_modified(false);
            *self.note.borrow_mut() = None;
            self.update_title();
        }
        saved
    }

    /// Before the tab closes: asks about unsaved changes. True if it may close.
    pub async fn may_close(self: Rc<Self>) -> bool {
        if !self.is_modified() {
            return true;
        }
        self.asking.set(true);
        let message = format!("Save changes to “{}”?", self.display_name());
        let detail = "Your changes will be lost if you don't save them.";
        let choice = choose(&self.window, &message, detail, &["Cancel", "Don't Save", "Save"]).await;
        self.asking.set(false);
        match choice {
            Some(2) => self.save().await,
            Some(1) => true,
            _ => false,
        }
    }

    /// The tab is going away: forgets its draft and remembers where the cursor was.
    pub fn closing(&self) {
        self.discard_draft();
        self.remember_place();
    }

    fn modified_changed(self: &Rc<Self>) {
        if self.is_modified() {
            if self.draft.borrow().is_none() {
                *self.draft.borrow_mut() = Some(session::new_draft_name());
            }
            self.queue_draft();
        } else {
            self.discard_draft();
        }
        self.update_title();
        crate::window::store_soon();
    }

    /// Turns spell checking on or off for this tab. The first time, libspelling is loaded and
    /// told what to skip.
    pub fn set_spell_check(&self, on: bool) {
        if on && self.spelling.borrow().is_none() {
            // Source files are GtkSourceView's (it tags their code itself); the rest is nevis's.
            if self.buffer.language().is_none() {
                // libspelling skips text with this tag; GtkSourceView gives it to code itself
                // in source files, nevis to code and Persian in notes.
                const SKIP: &str = "gtksourceview:context-classes:no-spell-check";
                let table = self.buffer.tag_table();
                let tag = table.lookup(SKIP).unwrap_or_else(|| {
                    let tag = gtk::TextTag::new(Some(SKIP));
                    table.add(&tag);
                    tag
                });
                self.styler.skip_spelling(tag);
            }
            let Some(spelling) = Spelling::new(&self.buffer) else { return };
            if let Some(actions) = spelling.actions() {
                self.view.insert_action_group("spelling", Some(&actions));
            }
            *self.spelling.borrow_mut() = Some(spelling);
        }
        let spelling = self.spelling.borrow();
        let Some(spelling) = spelling.as_ref() else { return };
        spelling.set_enabled(on);
        // Corrections first in the right-click menu, then nevis's own items.
        let menu = gio::Menu::new();
        if on {
            menu.append_section(None, spelling.menu());
        }
        menu.append_section(None, &crate::window::menu_model());
        self.view.set_extra_menu(Some(&menu));
    }

    /// Looks at what is being typed once the current edit or move is done.
    fn queue_completion(&self) {
        if self.completing.replace(true) {
            return;
        }
        let me = self.me.clone();
        glib::idle_add_local_once(move || {
            if let Some(editor) = me.upgrade() {
                editor.completing.set(false);
                editor.update_completion();
            }
        });
    }

    /// Shows what fits what is being typed under the cursor, or nothing.
    fn update_completion(&self) {
        let found = (|| {
            if self.is_reading() || !self.styler.is_enabled() || self.buffer.has_selection() || !self.view.is_focus() {
                return None;
            }
            let cursor = self.buffer.iter_at_mark(&self.buffer.get_insert());
            let mut line = cursor;
            line.set_line_offset(0);
            let (context, back) = complete::context(&self.buffer.text(&line, &cursor, true))?;
            let start = cursor.offset() - back as i32;
            if self.dismissed.get() == Some(start) {
                return None;
            }
            let offers = self.offers(&context);
            (!offers.is_empty()).then_some((offers, cursor))
        })();
        let Some((offers, cursor)) = found else {
            if let Some(completion) = self.completion.borrow().as_ref() {
                completion.hide();
            }
            return;
        };
        let completion = self.completion.borrow_mut().get_or_insert_with(|| {
            let completion = Completion::new();
            self.layer.add_overlay(&completion.widget);
            let me = self.me.clone();
            completion.on_click(move || {
                if let Some(editor) = me.upgrade() {
                    editor.accept_completion();
                    editor.view.grab_focus();
                }
            });
            completion
        }).clone();
        // Under the cursor's line; above it when there's no room below.
        let rect = self.view.iter_location(&cursor);
        let (x, y) = self.view.buffer_to_window_coords(gtk::TextWindowType::Widget, rect.x(), rect.y() + rect.height());
        let point = self.view.compute_point(&self.layer, &gtk::graphene::Point::new(x as f32, y as f32));
        let (x, y) = point.map_or((x, y), |point| (point.x() as i32, point.y() as i32));
        let height = 34 * offers.len().min(complete::ROWS) as i32 + 12;
        let y = if y + height + 8 > self.layer.height() { y - rect.height() - height - 4 } else { y + 4 };
        let x = x.min(self.layer.width() - 330);
        completion.show(offers, x, y);
    }

    /// What fits `context`, best first.
    fn offers(&self, context: &Context) -> Vec<Offer> {
        match context {
            Context::Link(query) => self.link_offers(query),
            Context::Heading(note, query) => {
                let text = if note.is_empty() {
                    Some(self.text())
                } else {
                    let path = links::split(note).path;
                    self.find(&path).and_then(|file| file.load_contents(None::<&gio::Cancellable>).ok()).map(|(bytes, _)| String::from_utf8_lossy(&bytes).into_owned())
                };
                let headings: Vec<String> = text.map(|text| outline::headings(&text).into_iter().map(|heading| heading.text).collect()).unwrap_or_default();
                switcher::rank(query, &headings)
                    .into_iter()
                    .take(complete::ROWS)
                    .map(|i| Offer { markup: complete::markup(&headings[i], ""), text: headings[i].clone() })
                    .collect()
            }
            Context::Tag(query) => {
                let Some((root, _, _)) = self.vault() else { return Vec::new() };
                // Read in the background the first time: the list comes once that's done.
                let me = self.me.clone();
                let Some(notes) = vault::index(&root, move |_| {
                    if let Some(editor) = me.upgrade() {
                        editor.queue_completion();
                    }
                }) else {
                    return Vec::new();
                };
                let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
                for tag in notes.iter().flat_map(|note| note.tags.iter()) {
                    *counts.entry(tag.trim_start_matches('#').to_owned()).or_default() += 1;
                }
                let mut tags: Vec<(String, usize)> = counts.into_iter().collect();
                tags.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                let names: Vec<String> = tags.iter().map(|(tag, _)| tag.clone()).collect();
                switcher::rank(query, &names)
                    .into_iter()
                    .filter(|&i| names[i] != *query)
                    .take(complete::ROWS)
                    .map(|i| Offer { markup: complete::markup(&format!("#{}", names[i]), &tags[i].1.to_string()), text: names[i].clone() })
                    .collect()
            }
            Context::Callout(query) => {
                let kinds: Vec<String> = complete::CALLOUTS.iter().map(|kind| kind.to_string()).collect();
                switcher::rank(query, &kinds)
                    .into_iter()
                    .take(complete::ROWS)
                    .map(|i| Offer { markup: complete::markup(&kinds[i], ""), text: format!("{}] ", kinds[i]) })
                    .collect()
            }
        }
    }

    /// Notes and files of the vault for a `[[` link: notes by name (with their folders when
    /// another has that name too), and today's, tomorrow's and yesterday's daily notes.
    fn link_offers(&self, query: &str) -> Vec<Offer> {
        let Some((root, files, _)) = self.vault() else { return Vec::new() };
        let entries: Vec<(String, String, String)> = files
            .iter()
            .map(|path| {
                let (folder, file) = path.rsplit_once('/').unwrap_or(("", path));
                let name = if listing::is_note(file) { file.rsplit_once('.').map_or(file, |(stem, _)| stem) } else { file };
                (name.to_owned(), folder.to_owned(), path.clone())
            })
            .collect();
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for (name, _, _) in &entries {
            *seen.entry(name.to_lowercase()).or_default() += 1;
        }
        let mut offers = Vec::new();
        let lower = query.to_lowercase();
        if lower.chars().count() >= 2 {
            let settings = std::fs::read_to_string(root.join(".obsidian/daily-notes.json")).ok();
            for (word, days) in [("today", 0), ("tomorrow", 1), ("yesterday", -1)] {
                let Some(day) = glib::DateTime::now_local().ok().and_then(|now| now.add_days(days).ok()) else { continue };
                if !word.starts_with(&lower) {
                    continue;
                }
                let date = (day.year(), day.month() as u32, day.day_of_month() as u32);
                let path = daily::path(date, settings.as_deref(), &[]);
                let name = path.rsplit('/').next().unwrap_or(&path).trim_end_matches(".md").to_owned();
                offers.push(Offer { markup: complete::markup(&name, word), text: name });
            }
        }
        let names: Vec<String> = entries.iter().map(|(name, _, _)| name.clone()).collect();
        for i in switcher::rank(query, &names).into_iter().take(complete::ROWS) {
            let (name, folder, path) = &entries[i];
            let text = if seen.get(&name.to_lowercase()).copied().unwrap_or(0) > 1 { path.trim_end_matches(".md").to_owned() } else { name.clone() };
            offers.push(Offer { markup: complete::markup(name, folder), text });
        }
        // Notes' aliases too: chosen, the link names the note and shows the alias.
        if let Some(notes) = vault::index(&root, |_| {}) {
            let aliases: Vec<(String, &str)> = notes.iter().flat_map(|note| note.aliases.iter().map(|alias| (alias.clone(), note.path.as_str()))).collect();
            let words: Vec<String> = aliases.iter().map(|(alias, _)| alias.clone()).collect();
            for i in switcher::rank(query, &words).into_iter().take(complete::ROWS) {
                let (alias, path) = &aliases[i];
                let name = path.rsplit('/').next().unwrap_or(path).trim_end_matches(".md");
                let note = if seen.get(&name.to_lowercase()).copied().unwrap_or(0) > 1 { path.trim_end_matches(".md") } else { name };
                offers.push(Offer { markup: complete::markup(alias, &format!("\u{2192} {name}")), text: format!("{note}|{alias}") });
            }
        }
        offers.truncate(complete::ROWS);
        offers
    }

    fn step_completion(&self, by: i32) -> bool {
        if let Some(completion) = self.completion.borrow().as_ref() {
            completion.step(by);
        }
        true
    }

    /// Puts the chosen completion in place of what was typed for it.
    fn accept_completion(&self) -> bool {
        let Some(text) = self.completion.borrow().as_ref().and_then(|completion| completion.chosen()) else { return false };
        let cursor = self.buffer.iter_at_mark(&self.buffer.get_insert());
        let mut line = cursor;
        line.set_line_offset(0);
        let Some((context, back)) = complete::context(&self.buffer.text(&line, &cursor, true)) else { return false };
        let mut start = cursor;
        start.backward_chars(back as i32);
        let mut end = cursor;
        let mut ahead = cursor;
        ahead.forward_chars(2);
        let closed = self.buffer.text(&cursor, &ahead, true) == "]]";
        let link = matches!(context, Context::Link(_) | Context::Heading(..));
        self.buffer.begin_user_action();
        self.buffer.delete(&mut start, &mut end);
        self.buffer.insert(&mut start, &text);
        if link && !closed {
            self.buffer.insert(&mut start, "]]");
        } else if link {
            start.forward_chars(2);
        }
        self.buffer.place_cursor(&start);
        self.buffer.end_user_action();
        if let Some(completion) = self.completion.borrow().as_ref() {
            completion.hide();
        }
        true
    }

    /// Esc: closes the completion until something else is being typed.
    fn dismiss_completion(&self) -> bool {
        let cursor = self.buffer.iter_at_mark(&self.buffer.get_insert());
        let mut line = cursor;
        line.set_line_offset(0);
        if let Some((_, back)) = complete::context(&self.buffer.text(&line, &cursor, true)) {
            self.dismissed.set(Some(cursor.offset() - back as i32));
        }
        if let Some(completion) = self.completion.borrow().as_ref() {
            completion.hide();
        }
        true
    }

    /// With autosave on, saves a file a second after the last change to it.
    fn queue_autosave(self: &Rc<Self>) {
        if !settings::get().autosave || self.location().is_none() {
            return;
        }
        if let Some(waiting) = self.autosave_due.take() {
            waiting.remove();
        }
        let weak = Rc::downgrade(self);
        let waiting = glib::timeout_add_local_once(Duration::from_secs(1), move || {
            let Some(editor) = weak.upgrade() else { return };
            editor.autosave_due.take();
            if editor.is_modified() && !editor.asking.get() {
                glib::spawn_future_local(editor.save());
            }
        });
        *self.autosave_due.borrow_mut() = Some(waiting);
    }

    /// Writes the draft a second after typing stops.
    fn queue_draft(self: &Rc<Self>) {
        if !session::enabled() || self.draft_due.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_secs(1), move || {
            if let Some(editor) = weak.upgrade() {
                editor.draft_due.set(false);
                editor.keep_draft_now();
            }
        });
    }

    /// Writes the unsaved text to the draft now.
    pub fn keep_draft_now(&self) {
        if !session::enabled() || !self.is_modified() {
            return;
        }
        if let Some(name) = self.draft.borrow().as_deref() {
            session::write_draft(name, &self.text());
        }
    }

    fn discard_draft(&self) {
        if let Some(name) = self.draft.borrow_mut().take() {
            session::remove_draft(&name);
        }
    }

    /// How the tab is to be found next time.
    pub fn tab_state(&self) -> TabState {
        if let Some(state) = self.restore.borrow().as_ref() {
            return state.clone();
        }
        let cursor = self.buffer.iter_at_mark(&self.buffer.get_insert());
        TabState {
            path: self.location().and_then(|l| l.path()),
            draft: self.is_modified().then(|| self.draft.borrow().clone()).flatten(),
            line: cursor.line() as u32,
            column: cursor.line_offset() as u32,
            reading: self.is_reading(),
        }
    }

    /// The note's text, all of it: with what Live Preview hides. (GTK leaves invisible text
    /// out unless asked; every read of a buffer here asks.)
    pub fn text(&self) -> String {
        self.buffer.text(&self.buffer.start_iter(), &self.buffer.end_iter(), true).to_string()
    }

    /// The file's name; for a note not saved yet, its first line.
    pub fn display_name(&self) -> String {
        match self.location().and_then(|l| l.basename()) {
            Some(name) => name.to_string_lossy().into_owned(),
            None => self.untitled_name(),
        }
    }

    /// A note not saved yet is called by its first line of words (see `names::of_text`).
    fn untitled_name(&self) -> String {
        let draft = self.restore.borrow().as_ref().and_then(|s| s.draft.clone()).and_then(|d| session::read_draft(&d));
        let text = draft.unwrap_or_else(|| {
            let mut end = self.buffer.start_iter();
            end.forward_lines(6);
            self.buffer.text(&self.buffer.start_iter(), &end, true).to_string()
        });
        names::of_text(&text)
    }

    /// "• notes.md (new file)": the name, whether it has unsaved changes, and any note.
    pub fn title(&self) -> String {
        let mut title = self.display_name();
        if self.is_modified() {
            title.insert_str(0, "• ");
        }
        if let Some(note) = self.note.borrow().as_deref() {
            title.push_str(&format!(" ({note})"));
        }
        title
    }

    fn update_title(&self) {
        self.update_tab();
        if let Some(host) = self.host.upgrade() {
            host.title_changed(self);
        }
    }

    /// Shows the number Alt+number picks this tab by; `None` for a tab no number reaches.
    pub fn set_number(&self, number: Option<usize>) {
        self.tab_number.set_text(&number.map(|n| n.to_string()).unwrap_or_default());
        self.tab_number.set_visible(number.is_some());
    }

    fn update_tab(&self) {
        self.tab_title.set_text(&self.display_name());
        let unsaved = self.is_modified() || self.restore.borrow().as_ref().is_some_and(|s| s.draft.is_some());
        let dot = unsaved && !self.hovered.get();
        self.tab_close.set_text(if dot { "●" } else { "×" });
        if dot {
            self.tab_close.add_css_class("unsaved");
        } else {
            self.tab_close.remove_css_class("unsaved");
        }
        let path = self.location().map(|l| tilde(&l.parse_name()));
        self.tab.set_tooltip_text(path.as_deref());
    }
}

/// Where the user was, kept across a reload.
struct Reload {
    cursor: i32,
    edit_scroll: Option<f64>,
    read_scroll: Option<f64>,
}

/// For the background service: does a first window's one-time work ahead of time (GTK's
/// renderer, the text views and their styles, Latin and Persian fonts in each weight, the
/// highlighting rules), so the first file opened is as quick as later ones. Nothing is shown:
/// the window is realized but never mapped.
pub fn warm_up() {
    let source = sourceview::View::new();
    source.add_css_class("nevis-editor");
    let reader = crate::paper::Paper::new();
    reader.add_css_class("nevis-read");
    // A tab bar with a tab, as windows with two tabs or more show.
    let tab = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tab.add_css_class("nevis-tab");
    tab.append(&gtk::Label::new(Some("notes.md")));
    tab.append(&gtk::Label::builder().label("×").css_classes(["nevis-tab-close"]).build());
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    bar.add_css_class("nevis-tabbar");
    bar.append(&tab);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&bar);
    content.append(&source);
    content.append(&reader);
    let window = gtk::Window::builder().child(&content).build();
    WidgetExt::realize(&window);
    for view in [&source, reader.upcast_ref()] {
        let layout = view.create_pango_layout(None);
        layout.set_markup("Text <b>bold</b> <i>italic</i> <tt>code</tt> فارسی <b>پررنگ</b> <i>کج</i> ۱۲۳");
        let _ = layout.pixel_size();
    }
    let _ = sourceview::LanguageManager::default().language("sh");
    devhooks::lap("warmed up");

    // Then the fonts other scripts fall back to, one per idle moment: finding and loading
    // each takes 100–300 ms, which would otherwise land on the first file that uses it.
    let mut samples = vec![
        "👋 🌍 🚀 ✅ ❤️ 👨‍👩‍👧 🇮🇷",
        "你好世界 こんにちは 안녕하세요",
        "नमस्ते दुनिया",
        "สวัสดีชาวโลก",
        "שלום עולם",
        "გამარჯობა Բարեւ",
        // Formulas ask for their font by name (theme::fonts::MATH).
        "<span font_family=\"DejaVu Math TeX Gyre, Noto Sans Math, DejaVu Serif, serif\">𝑥 + 𝑦 = ∑ √ ∫ ≤ ∞ <sup>2</sup></span>",
    ]
    .into_iter();
    glib::timeout_add_local(Duration::from_millis(250), move || {
        let Some(sample) = samples.next() else {
            window.destroy();
            devhooks::lap("fonts warmed up");
            return glib::ControlFlow::Break;
        };
        let layout = reader.create_pango_layout(None);
        layout.set_markup(sample);
        let _ = layout.pixel_size();
        glib::ControlFlow::Continue
    });
}

pub fn extension(file: &gio::File) -> Option<String> {
    let name = file.basename()?;
    Some(name.extension()?.to_str()?.to_ascii_lowercase())
}

pub fn is_markdown(file: &gio::File) -> bool {
    extension(file).is_some_and(|e| matches!(e.as_str(), "md" | "markdown" | "mdown" | "mkd" | "mkdn"))
}

pub fn is_text(file: &gio::File) -> bool {
    is_markdown(file) || extension(file).is_some_and(|e| e == "txt" || e == "text")
}

/// Opens a URI with the default app (browser, mail client, …).
fn launch(window: &gtk::ApplicationWindow, uri: &str) {
    gtk::UriLauncher::new(uri).launch(Some(window), None::<&gio::Cancellable>, |_| {});
}

/// Ctrl + scroll wheel zooms.
fn zoom_on_ctrl_scroll(scroller: &gtk::ScrolledWindow) {
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
    let pending = Cell::new(0.0);
    scroll.connect_scroll(move |controller, _, dy| {
        if !controller.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) {
            return glib::Propagation::Proceed;
        }
        // Touchpads send many small deltas; zoom one step per whole notch.
        pending.set(pending.get() + dy);
        while pending.get() <= -1.0 {
            crate::window::zoom(1.0);
            pending.set(pending.get() + 1.0);
        }
        while pending.get() >= 1.0 {
            crate::window::zoom(-1.0);
            pending.set(pending.get() - 1.0);
        }
        glib::Propagation::Stop
    });
    scroller.add_controller(scroll);
}

/// Runs `f` once, right after the window paints its next frame.
pub fn after_next_frame(window: &gtk::ApplicationWindow, f: impl FnOnce() + 'static) {
    let Some(clock) = window.frame_clock() else { return f() };
    let pending = RefCell::new(Some(f));
    let handler = Rc::new(RefCell::new(None));
    let handler_in = handler.clone();
    let id = clock.connect_after_paint(move |clock| {
        if let Some(f) = pending.borrow_mut().take() {
            if let Some(id) = handler_in.borrow_mut().take() {
                clock.disconnect(id);
            }
            glib::idle_add_local_once(f);
        }
    });
    *handler.borrow_mut() = Some(id);
    window.queue_draw();
}

/// "/home/soheil/notes" → "~/notes"
pub fn tilde(path: &str) -> String {
    let home = glib::home_dir();
    match home.to_str().and_then(|home| path.strip_prefix(home)) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_owned(),
    }
}

pub fn spawn<T: 'static>(future: impl Future<Output = T> + 'static) {
    glib::spawn_future_local(future);
}

/// Shows a modal question; returns the index of the chosen button (0 = cancel).
pub async fn choose(parent: &gtk::ApplicationWindow, message: &str, detail: &str, buttons: &[&str]) -> Option<i32> {
    gtk::AlertDialog::builder()
        .message(message)
        .detail(detail)
        .buttons(buttons)
        .cancel_button(0)
        .default_button(buttons.len() as i32 - 1)
        .modal(true)
        .build()
        .choose_future(Some(parent))
        .await
        .ok()
}

/// A side margin for a view `width` wide: with centred text (Settings → Text width), what
/// leaves a readable column in the middle, 60 ems of the text size (about 960 px at 12 pt);
/// never less than `least`.
fn side_margin(width: i32, least: i32) -> i32 {
    let now = settings::get();
    if !now.centered {
        return least;
    }
    let column = (60.0 * now.text_size * 96.0 / 72.0) as i32;
    ((width - column) / 2).max(least)
}

/// Backlinks found for a note: the folder searched, the note, and each note linking to it
/// (relative to the folder) with the lines that do.
struct Backlinks {
    root: gio::File,
    note: gio::File,
    found: Vec<(String, Vec<backlinks::Mention>)>,
}

pub fn alert(parent: &gtk::ApplicationWindow, message: &str, detail: &str) {
    gtk::AlertDialog::builder().message(message).detail(detail).modal(true).build().show(Some(parent));
}
