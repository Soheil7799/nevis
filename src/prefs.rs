//! The Settings page (Ctrl+,): every preference in one place, on a sheet over the window.
//! Each change applies at once; Esc or a click outside closes it.
//!
//! The controls are made of text and stylesheet only (a switch is a toggle button with a
//! knob), since GTK's own switch and drop-down load icons, which costs ~0.3 s the first time.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::{gio, glib, pango, prelude::*};

use crate::color::Rgb;
use crate::palettes::{Colors, Palette};
use crate::settings::{self, DEFAULT_TEXT_SIZE, Settings};
use crate::{popup, theme};

/// Brings a control back in line with the settings (shortcuts change them too).
type Refresh = Box<dyn Fn(&Settings)>;

pub struct SettingsPage {
    pub widget: gtk::Box,
    /// The mode switch's two halves: the one on gets the focus when the page opens.
    first: RefCell<Vec<gtk::ToggleButton>>,
    refreshers: RefCell<Vec<Refresh>>,
}

impl SettingsPage {
    pub fn new() -> Rc<Self> {
        let (widget, card) = popup::card(560);
        card.add_css_class("nevis-settings");
        card.set_margin_bottom(24);
        let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).margin_end(28).build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(620)
            .child(&body)
            .build();
        card.append(&gtk::Label::builder().label("Settings").xalign(0.0).margin_end(28).css_classes(["nevis-settings-title"]).build());
        card.append(&scroller);
        let page = Rc::new(Self { widget, first: RefCell::new(Vec::new()), refreshers: RefCell::new(Vec::new()) });
        page.follow_theme();

        section(&body, "Appearance");
        row(&body, "Mode", Some("Dark or light, in the version of the theme you chose last"), &page.mode());
        row(&body, "Theme", Some("Click one to use it"), &gtk::Box::new(gtk::Orientation::Horizontal, 0));
        body.append(&page.themes());
        row(&body, "Text color", Some("Kept readable on the theme's background"), &page.text_color());
        row(&body, "Text font", Some("For notes, rendered and in Live Preview"), &page.font(false));
        row(&body, "Code font", Some("For code, tables and the source view"), &page.font(true));
        row(&body, "Text size", None, &page.text_size());
        let width = page.choice(&[("Full width", false), ("Centered", true)], |s| s.centered, |s, v| s.centered = v);
        row(&body, "Text width", Some("Centered keeps lines to a readable length, in both views"), &width);

        section(&body, "Writing");
        row(&body, "Wrap long lines", None, &page.switch(|s| s.wrap, |s, on| s.wrap = on));
        row(&body, "Line numbers", Some("In the source view"), &page.switch(|s| s.line_numbers, |s, on| s.line_numbers = on));
        let lists = page.switch(|s| s.continue_lists, |s, on| s.continue_lists = on);
        row(&body, "Continue lists on Enter", Some("Lists, checklists and quotes carry on to the next line"), &lists);
        let opens = page.choice(&[("Rendered", true), ("As source", false)], |s| s.open_rendered, |s, v| s.open_rendered = v);
        row(&body, "Markdown files open", None, &opens);
        let editing = page.choice(&[("Live preview", true), ("Source", false)], |s| s.live_preview, |s, v| s.live_preview = v);
        row(&body, "Edit Markdown as", Some("Live preview hides the markup except on the line you're editing"), &editing);
        let breaks = page.choice(&[("Shown", false), ("Joined", true)], |s| s.strict_line_breaks, |s, v| s.strict_line_breaks = v);
        row(&body, "Single line breaks", Some("Shown, as in Obsidian, or joined into the paragraph, as on GitHub"), &breaks);
        let spelling = page.switch(|s| s.spell_check, |s, on| s.spell_check = on);
        row(&body, "Check spelling", Some("Right-click a word for corrections and the language. Code and Persian are skipped."), &spelling);
        let autosave = page.switch(|s| s.autosave, |s, on| s.autosave = on);
        row(&body, "Save automatically", Some("A second after you stop typing. New notes still ask for a name first."), &autosave);

        section(&body, "Windows and tabs");
        let outside = page.choice(&[("A new window", true), ("A tab", false)], |s| s.outside_in_window, |s, v| s.outside_in_window = v);
        row(&body, "Files opened from outside go in", Some("From a file manager or a terminal"), &outside);
        let bar = page.choice(&[("Two tabs or more", false), ("Always", true)], |s| s.tab_bar_always, |s, v| s.tab_bar_always = v);
        row(&body, "Show the tab bar with", None, &bar);
        page
    }

    pub fn is_open(&self) -> bool {
        self.widget.is_visible()
    }

    pub fn show(&self) {
        self.refresh();
        self.widget.set_visible(true);
        if let Some(on) = self.first.borrow().iter().find(|half| half.is_active()) {
            on.grab_focus();
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
        self.widget.set_visible(false);
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
        *self.first.borrow_mut() = vec![light.clone(), dark.clone()];
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
        // Tiles are made again only when the mode changes, not under the click that chose one.
        let tiles: Rc<RefCell<Vec<(gtk::Button, String)>>> = Rc::default();
        let made_for: Rc<Cell<Option<bool>>> = Rc::default();
        let shown = grid.clone();
        self.refreshers.borrow_mut().push(Box::new(move |_| {
            let dark = theme::is_dark();
            if made_for.replace(Some(dark)) != Some(dark) {
                while let Some(child) = shown.first_child() {
                    shown.remove(&child);
                }
                let mut made = tiles.borrow_mut();
                made.clear();
                for palette in theme::choices() {
                    if Colors::from(&palette).dark != dark {
                        continue;
                    }
                    let tile = theme_tile(&palette);
                    shown.append(&tile);
                    // One tab stop per tile, the button, where Enter and Space choose it.
                    if let Some(child) = tile.parent() {
                        child.set_focusable(false);
                    }
                    made.push((tile, palette.id));
                }
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

fn section(body: &gtk::Box, title: &str) {
    body.append(&gtk::Label::builder().label(title).xalign(0.0).css_classes(["nevis-settings-section"]).build());
}

/// A setting: its name (and a line about it) on the left, its control on the right.
fn row(body: &gtk::Box, name: &str, about: Option<&str>, control: &impl IsA<gtk::Widget>) {
    let words = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true).valign(gtk::Align::Center).build();
    words.append(&gtk::Label::builder().label(name).xalign(0.0).wrap(true).build());
    if let Some(about) = about {
        words.append(&gtk::Label::builder().label(about).xalign(0.0).wrap(true).css_classes(["nevis-settings-about"]).build());
    }
    let line = gtk::Box::builder().spacing(24).css_classes(["nevis-settings-row"]).build();
    line.append(&words);
    line.append(control);
    control.upcast_ref::<gtk::Widget>().update_property(&[gtk::accessible::Property::Label(name)]);
    body.append(&line);
}
