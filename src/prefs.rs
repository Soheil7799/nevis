//! The Settings window (Ctrl+,): a side menu of pages (Appearance, Writing, Windows and tabs,
//! About), each a column of cards. Each change applies at once; Esc closes it.
//!
//! The controls are made of text and stylesheet only (a switch is a toggle button with a
//! knob), since GTK's own switch and drop-down load icons, which costs ~0.3 s the first time.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::{gdk, gio, glib, pango, prelude::*};

use crate::color::Rgb;
use crate::palettes::{Colors, Palette};
use crate::settings::{self, DEFAULT_TEXT_SIZE, Settings};
use crate::theme;

/// Brings a control back in line with the settings (shortcuts change them too).
type Refresh = Box<dyn Fn(&Settings)>;

/// The pages, in the order the side menu lists them: (name, title).
const PAGES: &[(&str, &str)] = &[("appearance", "Appearance"), ("writing", "Writing"), ("windows", "Windows and tabs"), ("about", "About")];

pub struct SettingsPage {
    window: gtk::Window,
    /// The side menu: a row per page.
    nav: gtk::ListBox,
    refreshers: RefCell<Vec<Refresh>>,
}

impl SettingsPage {
    /// The Settings window over `parent`, hidden until shown.
    pub fn new(parent: &gtk::ApplicationWindow) -> Rc<Self> {
        let window = gtk::Window::builder()
            .title("Settings")
            .transient_for(parent)
            .hide_on_close(true)
            .default_width(860)
            .default_height(640)
            .css_classes(["nevis-prefs"])
            .build();
        let nav = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).vexpand(true).css_classes(["nevis-prefs-list"]).build();
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(120)
            .hexpand(true)
            .css_classes(["nevis-prefs-pages"])
            .build();
        let page = Rc::new(Self { window: window.clone(), nav: nav.clone(), refreshers: RefCell::new(Vec::new()) });
        page.follow_theme();

        let look = column();
        let theme = section(&look, "Theme");
        row(&theme, "Mode", Some("Dark or light, in the version of the theme you chose last"), &page.mode());
        row(&theme, "Theme", Some("Click one to use it. Add your own in ~/.config/nevis/themes"), &themes_folder()).add_css_class("joined");
        theme.append(&page.themes());
        let text = section(&look, "Text");
        row(&text, "Color", Some("Kept readable on the theme's background"), &page.text_color());
        row(&text, "Font", Some("For notes, rendered and in Live Preview"), &page.font(false));
        row(&text, "Code font", Some("For code, tables and the source view"), &page.font(true));
        row(&text, "Size", None, &page.text_size());
        let width = page.choice(&[("Full width", false), ("Centered", true)], |s| s.centered, |s, v| s.centered = v);
        row(&text, "Width", Some("Centered keeps lines to a readable length, in both views"), &width);
        stack.add_named(&scrolled("Appearance", &look), Some("appearance"));

        let writing = column();
        let editing = section(&writing, "Editing");
        row(&editing, "Wrap long lines", None, &page.switch(|s| s.wrap, |s, on| s.wrap = on));
        row(&editing, "Line numbers", Some("In the source view"), &page.switch(|s| s.line_numbers, |s, on| s.line_numbers = on));
        let lists = page.switch(|s| s.continue_lists, |s, on| s.continue_lists = on);
        row(&editing, "Continue lists on Enter", Some("Lists, checklists and quotes carry on to the next line"), &lists);
        let spelling = page.switch(|s| s.spell_check, |s, on| s.spell_check = on);
        row(&editing, "Check spelling", Some("Right-click a word for corrections and the language. Code and Persian are skipped."), &spelling);
        let autosave = page.switch(|s| s.autosave, |s, on| s.autosave = on);
        row(&editing, "Save automatically", Some("A second after you stop typing. New notes still ask for a name first."), &autosave);
        let markdown = section(&writing, "Markdown");
        let opens = page.choice(&[("Rendered", true), ("As source", false)], |s| s.open_rendered, |s, v| s.open_rendered = v);
        row(&markdown, "Files open", None, &opens);
        let edit_as = page.choice(&[("Live preview", true), ("Source", false)], |s| s.live_preview, |s, v| s.live_preview = v);
        row(&markdown, "Edit as", Some("Live preview hides the markup except on the line you're editing"), &edit_as);
        let breaks = page.choice(&[("Shown", false), ("Joined", true)], |s| s.strict_line_breaks, |s, v| s.strict_line_breaks = v);
        row(&markdown, "Single line breaks", Some("Shown, as in Obsidian, or joined into the paragraph, as on GitHub"), &breaks);
        stack.add_named(&scrolled("Writing", &writing), Some("writing"));

        let windows = column();
        let opening = section(&windows, "Opening files");
        let outside = page.choice(&[("A new window", true), ("A tab", false)], |s| s.outside_in_window, |s, v| s.outside_in_window = v);
        row(&opening, "Files opened from outside go in", Some("From a file manager or a terminal"), &outside);
        let tabs = section(&windows, "Tabs");
        let bar = page.choice(&[("Two tabs or more", false), ("Always", true)], |s| s.tab_bar_always, |s, v| s.tab_bar_always = v);
        row(&tabs, "Show the tab bar with", None, &bar);
        stack.add_named(&scrolled("Windows and tabs", &windows), Some("windows"));

        stack.add_named(&scrolled("", &about()), Some("about"));

        // The side menu, as the folder sidebar lists notes; the version under it opens About.
        let side = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["nevis-prefs-nav"]).build();
        side.append(&gtk::Label::builder().label("Settings").xalign(0.0).css_classes(["nevis-prefs-nav-title"]).build());
        for (_, title) in PAGES {
            nav.append(&gtk::Label::builder().label(*title).xalign(0.0).build());
        }
        let pages = stack.clone();
        nav.connect_row_selected(move |_, row| {
            if let Some((name, _)) = row.and_then(|row| PAGES.get(row.index() as usize)) {
                pages.set_visible_child_name(name);
            }
        });
        nav.select_row(nav.row_at_index(0).as_ref());
        side.append(&nav);
        let version = gtk::Label::builder().label(format!("nevis {}", env!("CARGO_PKG_VERSION"))).xalign(0.0).build();
        let foot = gtk::Button::builder().child(&version).tooltip_text("About nevis").css_classes(["nevis-prefs-foot"]).build();
        let menu = nav.clone();
        foot.connect_clicked(move |_| go_to(&menu, "about"));
        side.append(&foot);

        // "×" in the corner, over the pages.
        let close = gtk::Button::builder()
            .label("×")
            .tooltip_text("Close (Esc)")
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .css_classes(["nevis-prefs-close"])
            .build();
        close.update_property(&[gtk::accessible::Property::Label("Close Settings")]);
        let shut = window.clone();
        close.connect_clicked(move |_| shut.close());
        let pages = gtk::Overlay::builder().child(&stack).hexpand(true).build();
        pages.add_overlay(&close);
        let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        body.append(&side);
        body.append(&pages);
        window.set_child(Some(&body));
        // A click in the note puts Settings away (and goes on to the note). Not the note window
        // turning active: with focus following the mouse, passing over it does that.
        let click = gtk::GestureClick::builder().button(0).propagation_phase(gtk::PropagationPhase::Capture).build();
        let shut = window.downgrade();
        click.connect_pressed(move |_, _, _, _| {
            if let Some(window) = shut.upgrade()
                && window.is_visible()
            {
                window.close();
            }
        });
        parent.add_controller(click);

        // Esc closes; Ctrl+Page Up/Down and Alt+1…4 change the page.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let (shown, menu) = (window.clone(), nav.clone());
        keys.connect_key_pressed(move |_, key, keycode, modifiers| {
            let current = menu.selected_row().map_or(0, |row| row.index());
            let go = |index: i32| menu.select_row(menu.row_at_index(index.clamp(0, PAGES.len() as i32 - 1)).as_ref());
            let ctrl = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
            let alt = modifiers.contains(gdk::ModifierType::ALT_MASK);
            // The number row's 1…4 by position (evdev 2…5, +8), on any layout.
            let digit = (10..10 + PAGES.len() as u32).contains(&keycode).then(|| keycode as i32 - 10);
            match (key, digit) {
                (gdk::Key::Escape, _) => shown.close(),
                (gdk::Key::Page_Up, _) if ctrl => go(current - 1),
                (gdk::Key::Page_Down, _) if ctrl => go(current + 1),
                (_, Some(index)) if alt => go(index),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        window.add_controller(keys);
        page
    }

    pub fn is_open(&self) -> bool {
        self.window.is_visible()
    }

    pub fn show(&self) {
        self.refresh();
        self.window.present();
        if let Some(row) = self.nav.selected_row() {
            row.grab_focus();
        }
    }

    /// Shows the settings as they are now.
    pub fn refresh(&self) {
        let settings = settings::get();
        for refresh in self.refreshers.borrow().iter() {
            refresh(&settings);
        }
    }

    pub fn hide(&self) {
        self.window.close();
    }

    /// Shows the window at page `name` (see `PAGES`).
    pub fn show_page(&self, name: &str) {
        go_to(&self.nav, name);
        self.show();
    }

    /// An on/off switch for the setting `get` reads and `set` writes.
    fn switch(&self, get: fn(&Settings) -> bool, set: fn(&mut Settings, bool)) -> gtk::ToggleButton {
        let knob = gtk::Box::builder().css_classes(["knob"]).halign(gtk::Align::Start).valign(gtk::Align::Center).build();
        let toggle = gtk::ToggleButton::builder()
            .child(&knob)
            .css_classes(["nevis-switch"])
            .halign(gtk::Align::End)
            .valign(gtk::Align::Center)
            .active(get(&settings::get()))
            .build();
        toggle.connect_toggled(move |toggle| settings::change(|s| set(s, toggle.is_active())));
        let shown = toggle.clone();
        self.refreshers.borrow_mut().push(Box::new(move |s| shown.set_active(get(s))));
        toggle
    }

    /// One of a few `options` (label, value) for the setting `get` reads and `set` writes.
    fn choice<T: Copy + PartialEq + 'static>(&self, options: &[(&str, T)], get: fn(&Settings) -> T, set: fn(&mut Settings, T)) -> gtk::Box {
        let group = gtk::Box::builder().css_classes(["nevis-segments"]).halign(gtk::Align::End).valign(gtk::Align::Center).build();
        let mut buttons: Vec<(gtk::ToggleButton, T)> = Vec::new();
        for &(label, value) in options {
            let button = gtk::ToggleButton::builder().label(label).active(get(&settings::get()) == value).build();
            if let Some((first, _)) = buttons.first() {
                button.set_group(Some(first));
            }
            button.connect_toggled(move |button| {
                if button.is_active() {
                    settings::change(|s| set(s, value));
                }
            });
            group.append(&button);
            buttons.push((button, value));
        }
        self.refreshers.borrow_mut().push(Box::new(move |s| {
            for (button, value) in &buttons {
                if get(s) == *value {
                    button.set_active(true);
                }
            }
        }));
        group
    }

    /// "Light | Dark": the palette's kind, and a switch to the other.
    fn mode(self: &Rc<Self>) -> gtk::Box {
        let group = gtk::Box::builder().css_classes(["nevis-segments"]).halign(gtk::Align::End).valign(gtk::Align::Center).build();
        let light = gtk::ToggleButton::builder().label("Light").active(!theme::is_dark()).build();
        let dark = gtk::ToggleButton::builder().label("Dark").group(&light).active(theme::is_dark()).build();
        for (button, is_dark) in [(&light, false), (&dark, true)] {
            button.connect_toggled(move |button| {
                if button.is_active() {
                    theme::set_dark(is_dark);
                }
            });
            group.append(button);
        }
        self.refreshers.borrow_mut().push(Box::new(move |_| {
            if theme::is_dark() { dark.set_active(true) } else { light.set_active(true) }
        }));
        group
    }

    /// The palettes of the mode in use (System when the GTK theme is of it), each as a tile: a preview in its
    /// colours and its name. A click uses it; the page stays open, in the new colours.
    fn themes(self: &Rc<Self>) -> gtk::FlowBox {
        let grid = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .homogeneous(true)
            .min_children_per_line(3)
            .max_children_per_line(4)
            .column_spacing(8)
            .row_spacing(8)
            .margin_bottom(6)
            .css_classes(["nevis-themes"])
            .build();
        grid.update_property(&[gtk::accessible::Property::Label("Theme")]);
        // Tiles are made again only when the themes of the mode change (the mode, or a theme
        // file), not under the click that chose one.
        let tiles: Rc<RefCell<Vec<(gtk::Button, String)>>> = Rc::default();
        let made_for: Rc<RefCell<Vec<Palette>>> = Rc::default();
        let shown = grid.clone();
        self.refreshers.borrow_mut().push(Box::new(move |_| {
            let dark = theme::is_dark();
            let palettes: Vec<Palette> = theme::choices().into_iter().filter(|p| Colors::from(p).dark == dark).collect();
            if *made_for.borrow() != palettes {
                while let Some(child) = shown.first_child() {
                    shown.remove(&child);
                }
                let mut made = tiles.borrow_mut();
                made.clear();
                for palette in &palettes {
                    let tile = theme_tile(palette);
                    shown.append(&tile);
                    // One tab stop per tile, the button, where Enter and Space choose it.
                    if let Some(child) = tile.parent() {
                        child.set_focusable(false);
                    }
                    made.push((tile, palette.id.clone()));
                }
                *made_for.borrow_mut() = palettes;
            }
            let choice = theme::choice();
            for (tile, id) in tiles.borrow().iter() {
                if *id == choice { tile.add_css_class("on") } else { tile.remove_css_class("on") }
            }
        }));
        grid
    }

    /// The text colour: a dot in it, a way to pick another, and back to the theme's.
    fn text_color(self: &Rc<Self>) -> gtk::Box {
        let dot = gtk::Label::builder().use_markup(true).build();
        let change = gtk::Button::builder().label("Change…").css_classes(["nevis-settings-button"]).valign(gtk::Align::Center).build();
        let reset = gtk::Button::builder().label("Theme's").css_classes(["nevis-settings-button"]).valign(gtk::Align::Center).build();
        change.connect_clicked(|button| {
            let window = button.root().and_downcast::<gtk::Window>();
            // The colour chosen, not the one made readable from it: else each Select drifts.
            let now = theme::appearance().text_color.unwrap_or_else(|| theme::colors().text);
            let initial = gtk::gdk::RGBA::new(now.r as f32, now.g as f32, now.b as f32, 1.0);
            let dialog = gtk::ColorDialog::builder().title("Text Color").with_alpha(false).build();
            dialog.choose_rgba(window.as_ref(), Some(&initial), gio::Cancellable::NONE, |picked| {
                if let Ok(rgba) = picked {
                    let color = Rgb { r: f64::from(rgba.red()), g: f64::from(rgba.green()), b: f64::from(rgba.blue()), a: 1.0 };
                    theme::change_appearance(|a| a.text_color = Some(color));
                }
            });
        });
        reset.connect_clicked(|_| theme::change_appearance(|a| a.text_color = None));
        let both = gtk::Box::builder().spacing(10).halign(gtk::Align::End).build();
        both.append(&reset);
        both.append(&dot);
        both.append(&change);
        self.refreshers.borrow_mut().push(Box::new(move |_| {
            dot.set_markup(&format!("<span foreground=\"{}\">●</span>", theme::colors().text.to_hex()));
            reset.set_visible(theme::appearance().text_color.is_some());
        }));
        both
    }

    /// The text font, or with `code` the code font: its name in itself, a way to pick
    /// another (only monospace ones for code), and back to the default.
    fn font(self: &Rc<Self>, code: bool) -> gtk::Box {
        let name = gtk::Label::builder().use_markup(true).ellipsize(pango::EllipsizeMode::End).max_width_chars(18).build();
        let change = gtk::Button::builder().label("Change…").css_classes(["nevis-settings-button"]).valign(gtk::Align::Center).build();
        let reset = gtk::Button::builder().label("Default").css_classes(["nevis-settings-button"]).valign(gtk::Align::Center).build();
        change.connect_clicked(move |button| pick_font(button.root().and_downcast::<gtk::Window>().as_ref(), code));
        reset.connect_clicked(move |_| theme::change_appearance(|a| if code { a.code_font = None } else { a.text_font = None }));
        let both = gtk::Box::builder().spacing(10).halign(gtk::Align::End).build();
        both.append(&reset);
        both.append(&name);
        both.append(&change);
        self.refreshers.borrow_mut().push(Box::new(move |_| {
            let appearance = theme::appearance();
            let chosen = if code { appearance.code_font } else { appearance.text_font };
            let default = if code { theme::fonts::DEFAULT_MONO } else { theme::fonts::DEFAULT_BODY };
            let family = chosen.clone().unwrap_or_else(|| default.to_owned());
            let family = glib::markup_escape_text(&family);
            name.set_markup(&format!("<span font_family=\"{family}\">{family}</span>"));
            reset.set_visible(chosen.is_some());
        }));
        both
    }

    /// Shows the page again whenever the colours or fonts change while it is open; stops
    /// once the page is gone.
    fn follow_theme(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        theme::on_change(move || {
            let Some(page) = weak.upgrade() else { return false };
            if page.is_open() {
                page.refresh();
            }
            true
        });
    }

    /// "−  12 pt  +", and a way back to the default size.
    fn text_size(&self) -> gtk::Box {
        let minus = gtk::Button::builder().label("−").tooltip_text("Smaller (Ctrl+−)").build();
        let plus = gtk::Button::builder().label("+").tooltip_text("Larger (Ctrl++)").build();
        let value = gtk::Label::builder().width_chars(6).css_classes(["nevis-stepper-value"]).build();
        minus.connect_clicked(|_| settings::change(|s| s.text_size -= 1.0));
        plus.connect_clicked(|_| settings::change(|s| s.text_size += 1.0));
        let stepper = gtk::Box::builder().css_classes(["nevis-segments", "nevis-stepper"]).valign(gtk::Align::Center).build();
        stepper.append(&minus);
        stepper.append(&value);
        stepper.append(&plus);
        let reset = gtk::Button::builder().label("Reset").css_classes(["nevis-settings-button"]).valign(gtk::Align::Center).build();
        reset.connect_clicked(|_| settings::change(|s| s.text_size = DEFAULT_TEXT_SIZE));
        let both = gtk::Box::builder().spacing(10).halign(gtk::Align::End).build();
        both.append(&reset);
        both.append(&stepper);
        let (shown, again) = (value.clone(), reset.clone());
        self.refreshers.borrow_mut().push(Box::new(move |s| {
            shown.set_text(&format!("{} pt", s.text_size));
            again.set_visible(s.text_size != DEFAULT_TEXT_SIZE);
        }));
        both
    }
}

/// "Open folder": the user's themes folder in the file manager, made (with a README on the
/// format) the first time.
fn themes_folder() -> gtk::Button {
    let open = gtk::Button::builder().label("Open folder").css_classes(["nevis-settings-button"]).valign(gtk::Align::Center).build();
    open.connect_clicked(|button| {
        let dir = theme::themes_dir();
        let readme = dir.join("README.md");
        if std::fs::create_dir_all(&dir).is_ok() && !readme.exists() {
            let _ = std::fs::write(&readme, include_str!("../data/themes/README.md"));
        }
        let window = button.root().and_downcast::<gtk::Window>();
        gtk::FileLauncher::new(Some(&gio::File::for_path(&dir))).launch(window.as_ref(), gio::Cancellable::NONE, |_| {});
    });
    open
}

/// Asks for the text font, or with `code` the code font (monospace ones only), and uses it.
fn pick_font(window: Option<&gtk::Window>, code: bool) {
    let dialog = gtk::FontDialog::builder().title(if code { "Code Font" } else { "Text Font" }).build();
    if code {
        // The dialog lists families and their faces.
        dialog.set_filter(Some(&gtk::CustomFilter::new(|item| {
            item.downcast_ref::<pango::FontFamily>()
                .map(|family| family.is_monospace())
                .or_else(|| item.downcast_ref::<pango::FontFace>().map(|face| face.family().is_monospace()))
                .unwrap_or(true)
        })));
    }
    dialog.choose_family(window, None::<&pango::FontFamily>, gio::Cancellable::NONE, move |picked| {
        if let Ok(family) = picked {
            let font = Some(family.name().to_string());
            theme::change_appearance(|a| if code { a.code_font = font } else { a.text_font = font });
        }
    });
}

/// A palette as a button: a little page in its colours ("Aa" in its text colour, a line in
/// its quiet colour, its accents as dots), its name under it.
fn theme_tile(palette: &Palette) -> gtk::Button {
    let preview = gtk::DrawingArea::builder().content_width(104).content_height(58).build();
    let p = palette.clone();
    preview.set_draw_func(move |_, cr, width, height| {
        let (w, h) = (f64::from(width), f64::from(height));
        let paint = |c: Rgb| cr.set_source_rgb(c.r, c.g, c.b);
        rounded(cr, 0.5, 0.5, w - 1.0, h - 1.0, 9.0);
        paint(p.background);
        let _ = cr.fill_preserve();
        paint(p.background.mix(p.foreground, 0.2));
        cr.set_line_width(1.0);
        let _ = cr.stroke();
        // A line of text in the quiet colour, and the accents as dots.
        paint(p.comment);
        rounded(cr, 52.0, 18.0, w - 64.0, 5.0, 2.5);
        let _ = cr.fill();
        for (i, c) in [p.accent, p.red, p.green, p.blue, p.purple].into_iter().enumerate() {
            paint(c);
            cr.arc(17.0 + i as f64 * 14.0, h - 14.0, 4.5, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
        }
    });
    // "Aa" in the palette's text colour, over the little page.
    let sample = gtk::Label::builder()
        .use_markup(true)
        .label(format!("<span foreground=\"{}\" font_weight=\"600\" size=\"14pt\">Aa</span>", palette.foreground.to_hex()))
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .margin_start(11)
        .margin_top(5)
        .can_target(false)
        .build();
    let page = gtk::Overlay::builder().child(&preview).build();
    page.add_overlay(&sample);
    let name = gtk::Label::builder().label(&palette.name).ellipsize(pango::EllipsizeMode::End).css_classes(["nevis-theme-name"]).build();
    let both = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
    both.append(&page);
    both.append(&name);
    let tile = gtk::Button::builder().child(&both).tooltip_text(&palette.name).css_classes(["nevis-theme-tile"]).build();
    let id = palette.id.clone();
    tile.connect_clicked(move |_| theme::choose(&id, true));
    tile
}

/// A rectangle with round corners of `radius`, as the current path.
fn rounded(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, radius: f64) {
    use std::f64::consts::{FRAC_PI_2, PI};
    cr.new_sub_path();
    cr.arc(x + w - radius, y + radius, radius, -FRAC_PI_2, 0.0);
    cr.arc(x + w - radius, y + h - radius, radius, 0.0, FRAC_PI_2);
    cr.arc(x + radius, y + h - radius, radius, FRAC_PI_2, PI);
    cr.arc(x + radius, y + radius, radius, PI, 3.0 * FRAC_PI_2);
    cr.close_path();
}

/// The side menu's row for page `name` selected, and so its page shown.
fn go_to(nav: &gtk::ListBox, name: &str) {
    if let Some(index) = PAGES.iter().position(|(n, _)| *n == name) {
        nav.select_row(nav.row_at_index(index as i32).as_ref());
    }
}

/// A page's sections, one under the other.
fn column() -> gtk::Box {
    gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["nevis-prefs-page"]).build()
}

/// A page: its title over its sections, a column of their own width in the middle (not
/// stretched across a wide window), scrolling when the window is short.
fn scrolled(title: &str, content: &gtk::Box) -> gtk::ScrolledWindow {
    if !title.is_empty() {
        content.prepend(&gtk::Label::builder().label(title).xalign(0.0).css_classes(["nevis-prefs-heading"]).build());
    }
    content.set_halign(gtk::Align::Center);
    content.set_valign(gtk::Align::Start);
    content.set_width_request(600);
    gtk::ScrolledWindow::builder().child(content).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build()
}

/// About nevis: its mark, its name, who made it, what it is, and the versions (selectable, to
/// paste into a report).
fn about() -> gtk::Box {
    let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["nevis-about"]).build();
    let svg = glib::Bytes::from_static(include_bytes!("../data/dev.shirvani.Nevis.svg"));
    if let Ok(mark) = gdk::Texture::from_bytes(&svg) {
        body.append(&gtk::Image::builder().paintable(&mark).pixel_size(88).margin_bottom(16).build());
    }
    body.append(&gtk::Label::builder().label("nevis").css_classes(["nevis-about-name"]).build());
    let by = gtk::Label::builder().use_markup(true).label("by <a href=\"https://shirvani.dev\">shirvani.dev</a>").css_classes(["nevis-about-by"]).build();
    body.append(&by);
    let what = gtk::Label::builder()
        .label("A fast, minimal text and Markdown editor.")
        .justify(gtk::Justification::Center)
        .wrap(true)
        .max_width_chars(44)
        .margin_top(14)
        .build();
    body.append(&what);
    let links = gtk::Label::builder()
        .use_markup(true)
        .label("<a href=\"https://github.com/Soheil7799/nevis\">Source code</a>  ·  MIT license")
        .css_classes(["nevis-about-line"])
        .margin_top(14)
        .build();
    body.append(&links);
    let gtk_version = format!("{}.{}.{}", gtk::major_version(), gtk::minor_version(), gtk::micro_version());
    let versions = gtk::Label::builder()
        .label(format!("nevis {} · GTK {gtk_version}", env!("CARGO_PKG_VERSION")))
        .selectable(true)
        .css_classes(["nevis-about-version"])
        .margin_top(22)
        .build();
    body.append(&versions);
    let credit = gtk::Label::builder()
        .use_markup(true)
        .label("The pen nib is Font Awesome Free's, <a href=\"https://fontawesome.com/license/free\">CC BY 4.0</a>")
        .css_classes(["nevis-about-line", "nevis-about-credit"])
        .margin_top(6)
        .build();
    body.append(&credit);
    body
}

/// A section's title, and the card its rows go in.
fn section(body: &gtk::Box, title: &str) -> gtk::Box {
    body.append(&gtk::Label::builder().label(title).xalign(0.0).css_classes(["nevis-settings-section"]).build());
    let group = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["nevis-settings-group"]).build();
    body.append(&group);
    group
}

/// A setting: its name (and a line about it) on the left, its control on the right.
fn row(body: &gtk::Box, name: &str, about: Option<&str>, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let words = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true).valign(gtk::Align::Center).build();
    words.append(&gtk::Label::builder().label(name).xalign(0.0).wrap(true).build());
    if let Some(about) = about {
        words.append(&gtk::Label::builder().label(about).xalign(0.0).wrap(true).max_width_chars(52).css_classes(["nevis-settings-about"]).build());
    }
    let line = gtk::Box::builder().spacing(24).css_classes(["nevis-settings-row"]).build();
    line.append(&words);
    line.append(control);
    control.upcast_ref::<gtk::Widget>().update_property(&[gtk::accessible::Property::Label(name)]);
    body.append(&line);
    line
}
