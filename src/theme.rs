//! Colours and fonts: the palette in use (the GTK theme's own, "System", or a built-in one),
//! the GtkSourceView style scheme and stylesheet made from it, and the zoom.
//!
//! System follows the GTK stylesheet live: when matugen or noctalia rewrites it for a new
//! wallpaper, open windows repaint.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gtk::{gdk, gio, glib, prelude::*};
use sourceview::StyleSchemeManager;

use crate::color::Rgb;
use crate::gtkcolors::Named;
use crate::palettes::{self, Colors, Palette};
use crate::settings;

/// The palette that follows the GTK theme.
pub const SYSTEM: &str = "system";
const SCHEME_ID: &str = "nevis";


/// Fonts by role: the ones chosen in Settings, or these. Vazirmatn follows the code font:
/// without it Persian falls back to a monospace font that draws the letters unjoined.
pub mod fonts {
    pub const DEFAULT_MONO: &str = "JetBrains Mono";
    pub const DEFAULT_BODY: &str = "Inter";
    /// Formulas: fonts that have the mathematical italic letters (𝑥) and the symbols.
    pub const MATH: &str = "DejaVu Math TeX Gyre, Noto Sans Math, DejaVu Serif, serif";

    /// The code font, for the source view, code and tables.
    pub fn mono() -> String {
        super::with_appearance(|a| match &a.code_font {
            Some(font) => format!("{font}, Vazirmatn, monospace"),
            None => "JetBrains Mono, Vazirmatn, monospace".to_owned(),
        })
    }

    /// The code font for a stylesheet, before Vazirmatn and the generic family.
    pub fn mono_css() -> String {
        super::with_appearance(|a| a.code_font.as_ref().map_or_else(|| "\"JetBrains Mono\"".to_owned(), |font| format!("\"{font}\"")))
    }

    /// The text font for a stylesheet, before Vazirmatn and the generic family.
    pub fn body_css() -> String {
        super::with_appearance(|a| a.text_font.as_ref().map_or_else(|| "\"Inter Variable\", \"Inter\"".to_owned(), |font| format!("\"{font}\"")))
    }

    /// The rendered view's text (also Live Preview's).
    pub fn body() -> String {
        super::with_appearance(|a| match &a.text_font {
            Some(font) => format!("{font}, Vazirmatn, sans-serif"),
            None => "Inter Variable, Inter, Vazirmatn, sans-serif".to_owned(),
        })
    }
}

/// Choices on top of the palette: the text colour and the fonts, and the light and the dark
/// palette the mode switch goes back to. Kept in `~/.config/nevis/appearance`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Appearance {
    pub text_color: Option<Rgb>,
    pub text_font: Option<String>,
    pub code_font: Option<String>,
    pub light: Option<String>,
    pub dark: Option<String>,
}

/// Light and dark versions of the same palette, for the mode switch.
const PAIRS: &[(&str, &str)] = &[
    ("adwaita-light", "adwaita-dark"),
    ("ayu-light", "ayu-mirage"),
    ("catppuccin-latte", "catppuccin-mocha"),
    ("github-light", "github-dark"),
    ("gruvbox-light", "gruvbox-dark"),
    ("rose-pine-dawn", "rose-pine"),
    ("solarized-light", "solarized-dark"),
];

thread_local! {
    static FONT_CSS: gtk::CssProvider = gtk::CssProvider::new();
    /// nevis's own widgets, in the palette's colours.
    static CHROME_CSS: gtk::CssProvider = gtk::CssProvider::new();
    /// GTK's widgets (menus, fields, buttons) in a built-in palette's colours; empty for
    /// System, where the GTK theme already paints them.
    static WIDGET_CSS: gtk::CssProvider = gtk::CssProvider::new();
    /// The palette chosen (an id, or SYSTEM) and the colours worked out from it.
    static CHOICE: RefCell<String> = RefCell::new(SYSTEM.to_owned());
    static COLORS: RefCell<Option<Rc<Colors>>> = const { RefCell::new(None) };
    /// Called after the colours change.
    static LISTENERS: RefCell<Vec<Rc<dyn Fn() -> bool>>> = const { RefCell::new(Vec::new()) };
    /// Watching the GTK stylesheets, for System.
    static MONITORS: RefCell<Vec<gio::FileMonitor>> = const { RefCell::new(Vec::new()) };
    static RELOAD_PENDING: Cell<bool> = const { Cell::new(false) };
    static APPEARANCE: RefCell<Option<Appearance>> = const { RefCell::new(None) };
}

/// Works out the colours, registers the style scheme and the stylesheets. Call once at startup.
pub fn init() {
    if let Some(dir) = styles_dir().to_str() {
        StyleSchemeManager::default().append_search_path(dir);
    }
    // Left over from before palettes could change.
    let _ = std::fs::remove_file(styles_dir().join("nevis-ayu-mirage.xml"));
    let Some(display) = gdk::Display::default() else { return };
    // Above everything else, the user's GTK stylesheet and the palette's widget colours included:
    // both style buttons everywhere, and nevis's own controls (tabs, switches) are buttons too.
    CHROME_CSS.with(|css| gtk::style_context_add_provider_for_display(&display, css, gtk::STYLE_PROVIDER_PRIORITY_USER + 2));
    // Above the user's own GTK stylesheet, which would otherwise win.
    WIDGET_CSS.with(|css| gtk::style_context_add_provider_for_display(&display, css, gtk::STYLE_PROVIDER_PRIORITY_USER + 1));
    FONT_CSS.with(|css| gtk::style_context_add_provider_for_display(&display, css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION));
    apply_font();

    let choice = std::fs::read_to_string(choice_file()).map(|text| text.trim().to_owned()).unwrap_or_default();
    let known = choice == SYSTEM || find(&choice).is_some();
    CHOICE.with_borrow_mut(|current| *current = if known { choice } else { SYSTEM.to_owned() });
    apply(false);
    if let Some(settings) = gtk::Settings::default() {
        settings.connect_gtk_application_prefer_dark_theme_notify(|_| schedule_reload());
        settings.connect_gtk_theme_name_notify(|_| schedule_reload());
    }
}

/// The colours in use.
pub fn colors() -> Rc<Colors> {
    COLORS.with_borrow(|colors| colors.clone()).unwrap_or_else(|| {
        let colors = Rc::new(Colors::from(&palette(SYSTEM)));
        COLORS.with_borrow_mut(|slot| *slot = Some(colors.clone()));
        colors
    })
}

/// Runs `f` with `colors` in use instead of the palette's (printing: dark on white paper).
pub fn with_colors<R>(colors: Rc<Colors>, f: impl FnOnce() -> R) -> R {
    let before = COLORS.with_borrow_mut(|slot| slot.replace(colors));
    let result = f();
    COLORS.with_borrow_mut(|slot| *slot = before);
    result
}

/// The colours for paper, whatever the palette: GitHub Light's, on white.
pub fn paper_colors() -> Rc<Colors> {
    let palette = palettes::built_in().into_iter().find(|p| p.id == "github-light").expect("GitHub Light is built in");
    Rc::new(Colors::from(&palette))
}

pub fn scheme() -> Option<sourceview::StyleScheme> {
    StyleSchemeManager::default().scheme(SCHEME_ID)
}

/// The palette chosen: an id, or SYSTEM.
pub fn choice() -> String {
    CHOICE.with_borrow(|choice| choice.clone())
}

/// Shows `id`'s palette everywhere; `keep` remembers it for next time too.
pub fn choose(id: &str, keep: bool) {
    show(id, keep);
    if keep && id != SYSTEM {
        // The mode switch comes back to this palette, and goes to its other version first.
        let dark = colors().dark;
        let partner = partner(id);
        change_appearance(|a| {
            let (same, other) = if dark { (&mut a.dark, &mut a.light) } else { (&mut a.light, &mut a.dark) };
            *same = Some(id.to_owned());
            if let Some(partner) = partner {
                *other = Some(partner.to_owned());
            }
        });
    }
}

/// Shows `id`'s palette everywhere; `keep` remembers it for next time too.
fn show(id: &str, keep: bool) {
    let before = CHOICE.with_borrow_mut(|choice| std::mem::replace(choice, id.to_owned()));
    if keep {
        let file = choice_file();
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(file, format!("{id}\n"));
    }
    // Another palette in the same colours (a copy of a built-in one) repaints nothing, but
    // Settings still marks the one chosen.
    if !apply(true) && before != id {
        tell();
    }
}

/// The other version, light or dark, of the palette `id`.
fn partner(id: &str) -> Option<&'static str> {
    PAIRS.iter().find_map(|&(light, dark)| if id == light { Some(dark) } else if id == dark { Some(light) } else { None })
}

/// The palette in use is a dark one.
pub fn is_dark() -> bool {
    colors().dark
}

/// Goes to a dark palette, or a light one: the one last chosen of that kind, the other version
/// of this palette, or Adwaita's.
pub fn set_dark(dark: bool) {
    if is_dark() == dark {
        return;
    }
    // Coming back to this mode comes back to this palette, whether it has a partner or not.
    let leaving = choice();
    change_appearance(|a| *(if dark { &mut a.light } else { &mut a.dark }) = Some(leaving));
    let remembered = if dark { appearance().dark } else { appearance().light };
    let known = |id: &String| id == SYSTEM || find(id).is_some();
    let fallback = if dark { "adwaita-dark" } else { "adwaita-light" };
    let id = remembered.filter(known).or_else(|| partner(&choice()).map(str::to_owned)).unwrap_or_else(|| fallback.to_owned());
    show(&id, true);
}

/// The text colour, fonts and remembered palettes.
pub fn appearance() -> Appearance {
    with_appearance(Appearance::clone)
}

/// `f` of the appearance, read from its file the first time.
fn with_appearance<R>(f: impl FnOnce(&Appearance) -> R) -> R {
    if APPEARANCE.with_borrow(Option::is_none) {
        APPEARANCE.set(Some(parse_appearance(&std::fs::read_to_string(appearance_file()).unwrap_or_default())));
    }
    APPEARANCE.with_borrow(|a| f(a.as_ref().expect("just read")))
}

/// Changes the text colour, the fonts or the remembered palettes, keeps them for next time,
/// and repaints.
pub fn change_appearance(edit: impl FnOnce(&mut Appearance)) {
    let old = appearance();
    let mut new = old.clone();
    edit(&mut new);
    if new == old {
        return;
    }
    APPEARANCE.set(Some(new.clone()));
    write_atomically(&appearance_file(), &appearance_text(&new));
    let fonts = new.text_font != old.text_font || new.code_font != old.code_font;
    if fonts {
        apply_font();
    }
    if new.text_color != old.text_color {
        apply(true);
    } else if fonts {
        tell();
    }
}

fn appearance_file() -> PathBuf {
    glib::user_config_dir().join("nevis").join("appearance")
}

/// "name = value" lines; unknown names and values that don't read are left out.
fn parse_appearance(text: &str) -> Appearance {
    let mut appearance = Appearance::default();
    for line in text.lines() {
        let Some((name, value)) = line.split_once('=') else { continue };
        let (name, value) = (name.trim(), value.trim());
        let named = (!value.is_empty()).then(|| value.to_owned());
        match name {
            "text-color" => appearance.text_color = Rgb::hex(value),
            "text-font" => appearance.text_font = named,
            "code-font" => appearance.code_font = named,
            "light" => appearance.light = named,
            "dark" => appearance.dark = named,
            _ => {}
        }
    }
    appearance
}

fn appearance_text(a: &Appearance) -> String {
    let mut text = String::new();
    let mut line = |name: &str, value: Option<String>| {
        if let Some(value) = value {
            text.push_str(&format!("{name} = {value}\n"));
        }
    };
    line("text-color", a.text_color.map(Rgb::to_hex));
    line("text-font", a.text_font.clone());
    line("code-font", a.code_font.clone());
    line("light", a.light.clone());
    line("dark", a.dark.clone());
    text
}

/// Every palette that can be chosen: System first, then the user's own, then the built-in
/// ones (a user's theme of the same id stands in for a built-in one).
pub fn choices() -> Vec<Palette> {
    let mut own = user_themes();
    let built_in: Vec<Palette> = palettes::built_in().into_iter().filter(|p| !own.iter().any(|o| o.id == p.id)).collect();
    // A copy of a built-in theme still named as it ("Nord") says which file it is.
    for theme in &mut own {
        if built_in.iter().any(|p| p.name == theme.name) {
            theme.name = format!("{} ({})", theme.name, theme.id);
        }
    }
    std::iter::once(palette(SYSTEM)).chain(own).chain(built_in).collect()
}

/// The user's own themes: `~/.config/nevis/themes/<id>.theme`, in the format of the built-in
/// ones (`data/themes`), so that a generator (matugen) can write one.
pub fn themes_dir() -> PathBuf {
    glib::user_config_dir().join("nevis").join("themes")
}

/// The palette `id` names, the user's own before a built-in one; none for System.
fn find(id: &str) -> Option<Palette> {
    user_theme(id).or_else(|| palettes::built_in().into_iter().find(|p| p.id == id))
}

fn user_theme(id: &str) -> Option<Palette> {
    if id == SYSTEM || id.is_empty() || id.contains(['/', '\\']) {
        return None;
    }
    palettes::read(id, &std::fs::read_to_string(themes_dir().join(format!("{id}.theme"))).ok()?)
}

/// Every theme in the user's folder that reads, by file name.
fn user_themes() -> Vec<Palette> {
    let Ok(entries) = std::fs::read_dir(themes_dir()) else { return Vec::new() };
    let mut ids: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.strip_suffix(".theme").map(str::to_owned))
        .collect();
    ids.sort();
    ids.iter().filter_map(|id| user_theme(id)).collect()
}

/// Runs `f` after every change of colours or fonts, for as long as it returns true.
pub fn on_change(f: impl Fn() -> bool + 'static) {
    LISTENERS.with_borrow_mut(|listeners| listeners.push(Rc::new(f)));
}

/// The palette `id` names; System is the GTK theme's, or Adwaita's when it names no colours.
fn palette(id: &str) -> Palette {
    if id != SYSTEM
        && let Some(palette) = find(id)
    {
        return palette;
    }
    let (named, _) = gtk_colors();
    let dark = gtk::Settings::default().is_none_or(|s| s.is_gtk_application_prefer_dark_theme());
    let fallback = if dark { "adwaita-dark" } else { "adwaita-light" };
    palettes::from_gtk(&named).unwrap_or_else(|| {
        let mut palette = palettes::built_in().into_iter().find(|p| p.id == fallback).expect("Adwaita is built in");
        (palette.id, palette.name) = (SYSTEM.to_owned(), "System".to_owned());
        palette
    })
}

/// Works the colours out again and repaints; `tell` lets open windows know.
/// Whether the colours changed (and, with `tell`, windows were told).
fn apply(tell: bool) -> bool {
    let choice = choice();
    let mut palette = palette(&choice);
    if let Some(color) = appearance().text_color {
        palette.foreground = color;
    }
    let colors = Rc::new(Colors::from(&palette));
    let changed = COLORS.with_borrow(|old| old.as_ref().is_none_or(|old| scheme_xml(old) != scheme_xml(&colors)));
    COLORS.with_borrow_mut(|slot| *slot = Some(colors.clone()));
    write_scheme(&colors);
    CHROME_CSS.with(|css| css.load_from_string(&chrome_css(&colors)));
    let widgets = if choice == SYSTEM { String::new() } else { widget_css(&colors) };
    WIDGET_CSS.with(|css| css.load_from_string(&widgets));
    watch(&choice);
    if tell && changed {
        self::tell();
    }
    changed
}

/// Lets open windows know the colours or fonts changed, and drops the listeners that are done.
fn tell() {
    let listeners = LISTENERS.with_borrow(|listeners| listeners.clone());
    let done: Vec<_> = listeners.into_iter().filter(|listener| !listener()).collect();
    LISTENERS.with_borrow_mut(|listeners| listeners.retain(|l| !done.iter().any(|d| Rc::ptr_eq(l, d))));
}

/// The GTK theme's named colours, and the stylesheets they come from.
fn gtk_colors() -> (Named, Vec<PathBuf>) {
    let dir = glib::user_config_dir().join("gtk-4.0");
    let main = dir.join("gtk.css");
    let mut files = vec![main.clone()];
    let css = std::fs::read_to_string(&main).unwrap_or_default();
    let named = Named::read(&css, &mut |url| {
        let path = url.strip_prefix("file://").map_or_else(|| dir.join(url), PathBuf::from);
        files.push(path.clone());
        std::fs::read_to_string(path).ok()
    });
    (named, files)
}

/// Repaints when what the colours come from changes: the GTK stylesheets while System is
/// chosen, the theme's file while one of the user's own is.
fn watch(choice: &str) {
    let changed = |event: gio::FileMonitorEvent| {
        matches!(
            event,
            gio::FileMonitorEvent::ChangesDoneHint | gio::FileMonitorEvent::Created | gio::FileMonitorEvent::Deleted | gio::FileMonitorEvent::MovedIn
        )
    };
    let monitors = if choice == SYSTEM {
        gtk_colors()
            .1
            .iter()
            .filter_map(|path| gio::File::for_path(path).monitor_file(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>).ok())
            .inspect(|monitor| {
                monitor.connect_changed(move |_, _, _, event| {
                    if changed(event) {
                        schedule_reload();
                    }
                });
            })
            .collect()
    } else if user_theme(choice).is_some() {
        // The folder, not the file: generators write a new file and rename it over the old.
        let name = format!("{choice}.theme");
        let folder = gio::File::for_path(themes_dir()).monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, None::<&gio::Cancellable>).ok();
        folder
            .inspect(|monitor| {
                monitor.connect_changed(move |_, file, other, event| {
                    let named = |f: &gio::File| f.basename().is_some_and(|base| base.as_os_str() == name.as_str());
                    if (named(file) || other.is_some_and(named)) && (changed(event) || event == gio::FileMonitorEvent::Renamed) {
                        schedule_reload();
                    }
                });
            })
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };
    MONITORS.with_borrow_mut(|old| *old = monitors);
}

/// Generators write the stylesheets in several steps; look once they are done.
fn schedule_reload() {
    if RELOAD_PENDING.replace(true) {
        return;
    }
    glib::timeout_add_local_once(Duration::from_millis(300), || {
        RELOAD_PENDING.set(false);
        // A built-in palette comes out the same, and then no one is told.
        apply(true);
    });
}

fn styles_dir() -> PathBuf {
    glib::user_data_dir().join("nevis").join("styles")
}

fn choice_file() -> PathBuf {
    glib::user_config_dir().join("nevis").join("theme")
}

/// GtkSourceView only reads schemes from files: write ours where it looks, when it changed.
fn write_scheme(colors: &Colors) {
    let xml = scheme_xml(colors);
    let path = styles_dir().join(format!("{SCHEME_ID}.xml"));
    if std::fs::read_to_string(&path).ok().as_deref() == Some(xml.as_str()) {
        return;
    }
    write_atomically(&path, &xml);
    StyleSchemeManager::default().force_rescan();
}

fn write_atomically(path: &Path, text: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let partial = path.with_extension("partial");
    if std::fs::write(&partial, text).is_ok() {
        let _ = std::fs::rename(&partial, path);
    }
}

fn scheme_xml(c: &Colors) -> String {
    let h = |color: Rgb| color.to_hex();
    let variant = if c.dark { "dark" } else { "light" };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!-- Written by nevis from its palette; changes are overwritten. -->
<style-scheme id="{SCHEME_ID}" name="nevis" version="1.0">
  <metadata><property name="variant">{variant}</property></metadata>
  <style name="text" foreground="{text}" background="{bg}"/>
  <style name="selection" background="{selection}"/>
  <style name="selection-unfocused" background="{selection}"/>
  <style name="cursor" foreground="{accent}"/>
  <style name="current-line" background="{line}"/>
  <style name="line-numbers" foreground="{marker}" background="{bg}"/>
  <style name="current-line-number" foreground="{text}" background="{line}"/>
  <style name="bracket-match" foreground="{accent}" bold="true"/>
  <style name="search-match" background="{search}"/>
  <style name="draw-spaces" foreground="{rule}"/>
  <style name="background-pattern" background="{line}"/>
  <style name="def:comment" foreground="{dim}" italic="true"/>
  <style name="def:doc-comment-element" foreground="{dim}" bold="true"/>
  <style name="def:shebang" foreground="{dim}" bold="true"/>
  <style name="def:constant" foreground="{number}"/>
  <style name="def:special-constant" foreground="{number}"/>
  <style name="def:number" foreground="{number}"/>
  <style name="def:boolean" foreground="{number}"/>
  <style name="def:character" foreground="{string}"/>
  <style name="def:string" foreground="{string}"/>
  <style name="def:special-char" foreground="{special}"/>
  <style name="def:identifier" foreground="{text}"/>
  <style name="def:function" foreground="{function}"/>
  <style name="def:builtin" foreground="{special}"/>
  <style name="def:statement" foreground="{keyword}"/>
  <style name="def:keyword" foreground="{keyword}"/>
  <style name="def:operator" foreground="{special}"/>
  <style name="def:type" foreground="{kind}"/>
  <style name="def:preprocessor" foreground="{keyword}"/>
  <style name="def:error" underline="error"/>
  <style name="def:warning" underline="error"/>
  <style name="def:note" foreground="{bg}" background="{accent}" bold="true"/>
  <style name="def:net-address" foreground="{link}" underline="single"/>
  <style name="def:underlined" underline="single"/>
  <style name="def:insertion" foreground="{added}"/>
  <style name="def:deletion" foreground="{removed}" strikethrough="true"/>
  <style name="def:heading" foreground="{heading}" bold="true"/>
  <style name="def:emphasis" italic="true"/>
  <style name="def:strong-emphasis" bold="true"/>
  <style name="def:inline-code" foreground="{code}"/>
  <style name="def:preformatted-section" foreground="{code}"/>
  <style name="def:list-marker" foreground="{bullet}" bold="true"/>
  <style name="def:thematic-break" foreground="{dim}"/>
  <style name="def:link-text" foreground="{link}"/>
  <style name="def:link-symbol" foreground="{dim}"/>
  <style name="def:link-destination" foreground="{dim}" italic="true"/>
  <style name="diff:added-line" foreground="{added}"/>
  <style name="diff:removed-line" foreground="{removed}"/>
  <style name="diff:changed-line" foreground="{changed}"/>
  <style name="diff:location" foreground="{number}"/>
</style-scheme>
"#,
        text = h(c.text),
        bg = h(c.background),
        selection = h(c.selection),
        accent = h(c.accent),
        line = h(c.current_line),
        marker = h(c.marker),
        search = h(c.search_match),
        rule = h(c.rule),
        dim = h(c.dim),
        number = h(c.number),
        string = h(c.string),
        special = h(c.special),
        function = h(c.function),
        keyword = h(c.keyword),
        kind = h(c.kind),
        link = h(c.link),
        added = h(c.added),
        removed = h(c.removed),
        changed = h(c.changed),
        heading = h(c.heading),
        code = h(c.code),
        bullet = h(c.bullet),
    )
}

/// nevis's own widgets, in loom's terms: the page a rounded island in the surface's frame, soft
/// fills instead of lines, one set of corners (6 rows, 8 controls, 10 cards, 12 panels, 999
/// pills), the accent kept for what is chosen or focused. Type steps: 0.8 labels, 0.9
/// secondary, 1 body, 1.2 titles.
fn chrome_css(c: &Colors) -> String {
    let h = |color: Rgb| color.to_hex();
    let (surface, accent, text, background) = (h(c.surface), h(c.accent), h(c.text), h(c.background));
    let error = h(c.error);
    // Secondary text: the text at about two thirds, yet 4.5:1 on the surface and the page.
    let muted = h(c.text.mix(c.surface, 0.32).readable_on(c.surface, 4.5).readable_on(c.background, 4.5));
    let on_accent = h(if c.accent.contrast(Rgb::BLACK) > c.accent.contrast(Rgb::WHITE) { Rgb::BLACK } else { Rgb::WHITE });
    format!(
        "/* The frame and the page: the window in the surface colour, the page a rounded panel in it. */
         .nevis-frame {{ background: {surface}; }}
         .nevis-island {{ background: {background}; border-radius: 12px; margin: 6px; }}
         /* The views in the page's colour all over: GTK paints a view's margins (the space left
            of the line numbers, with centred text) in the GTK theme's colour, not the text's. */
         textview.nevis-editor, textview.nevis-read, textview.nevis-editor > border, textview.nevis-read > border {{
             background-color: {background}; }}
         paned.nevis-main > separator {{ background: none; background-color: {surface}; color: transparent; box-shadow: none; border: none;
             border-image: none; outline: none; -gtk-icon-source: none; min-width: 1px; min-height: 1px; }}
         paned > separator {{ background: alpha({text}, 0.07); min-width: 1px; }}
         /* Keyboard focus is always visible. */
         .nevis-sidebar > row:focus-visible, .nevis-switcher-list > row:focus-visible, .nevis-settings-button:focus-visible,
         .nevis-stepper > button:focus-visible, .nevis-tab:focus-visible, .nevis-prefs-close:focus-visible {{ outline: 2px solid alpha({accent}, 0.7); outline-offset: -2px; }}
         .nevis-switch:focus-visible, .nevis-segments > button:focus-visible, .nevis-theme-tile:focus-visible {{
             outline: 2px solid alpha({accent}, 0.7); outline-offset: 2px; }}

         .nevis-find {{ padding: 6px 10px; background: alpha({text}, 0.035); border-bottom: 1px solid alpha({text}, 0.07); }}
         .nevis-table {{ border: none; background: none; }}
         .nevis-table-head, .nevis-table-cell {{ padding: 7px 22px 7px 0; }}
         .nevis-table-head {{ color: {muted}; border-bottom: 1px solid alpha({text}, 0.2); }}
         .nevis-table-cell {{ border-bottom: 1px solid alpha({text}, 0.07); }}
         .nevis-table-cell.last {{ border-bottom: none; }}

         /* The sidebar: rows in the frame, the chosen one tinted. */
         .nevis-sidebar, .nevis-sidebar-pane {{ background: {surface}; }}
         /* No padding above and below a row: its indent guides join up with the next row's. */
         .nevis-sidebar > row {{ padding: 0 10px 0 4px; border-radius: 6px; margin: 0 8px; color: {text}; }}
         .nevis-sidebar > row:hover {{ background: alpha({text}, 0.05); }}
         .nevis-sidebar > row:selected {{ background: alpha({accent}, 0.12); color: {text}; }}
         /* The user's GTK stylesheet may colour labels in rows itself; these follow the row. */
         .nevis-sidebar-name {{ padding: 4px 0; color: inherit; }}
         .nevis-sidebar-arrow, .nevis-file-icon {{ color: {muted}; }}
         .nevis-file-icon.folder, .nevis-sidebar > row:selected .nevis-file-icon {{ color: {accent}; }}
         .nevis-sidebar-header {{ padding: 12px 8px 6px 18px; }}
         .nevis-sidebar-title {{ color: {muted}; font-size: 0.8em; font-weight: 700; letter-spacing: 0.06em; text-transform: uppercase; }}

         /* Cards over the window: a soft edge and a shadow, no hard line. */
         .nevis-help-backdrop {{ background: alpha(black, 0.3); }}
         .nevis-switcher {{ background: {surface}; color: {text}; border: 1px solid alpha({text}, 0.12); border-radius: 12px;
                           padding: 8px; box-shadow: 0 6px 20px alpha(black, 0.35); }}
         /* The search field is the card's top line, not a box in it. */
         .nevis-switcher > entry {{ background: none; border: none; box-shadow: none; outline: none; border-radius: 0;
             padding: 6px 8px 10px 8px; font-size: 1.05em; border-bottom: 1px solid alpha({text}, 0.07); margin-bottom: 2px; }}
         .nevis-switcher-list {{ background: transparent; }}
         .nevis-preview, .nevis-preview > text {{ background: alpha({text}, 0.04); border-radius: 8px; font-size: 0.9em; }}
         .nevis-outline-top {{ font-weight: 600; }}
         .nevis-complete {{ background: {surface}; color: {text}; border: 1px solid alpha({text}, 0.12); border-radius: 10px; padding: 4px;
             box-shadow: 0 6px 20px alpha(black, 0.35); }}
         .nevis-complete-list {{ background: transparent; }}
         .nevis-complete-list > row {{ padding: 4px 8px; border-radius: 6px; min-height: 0; }}
         .nevis-outline-empty {{ color: {muted}; padding: 6px 2px; }}
         .nevis-toast {{ background: {surface}; color: {text}; border: 1px solid alpha({text}, 0.12); border-radius: 10px; padding: 6px 16px;
             box-shadow: 0 6px 20px alpha(black, 0.35); }}
         /* The user's GTK stylesheet may colour labels in rows itself; these follow the row. */
         .nevis-switcher-list > row, .nevis-complete-list > row {{ color: {text}; }}
         .nevis-switcher-list > row label:not(.nevis-help-keys), .nevis-complete-list > row label {{ color: inherit; }}
         .nevis-switcher-list > row {{ padding: 7px 10px; border-radius: 8px; }}
         /* The user's GTK stylesheet may paint list rows in its own (dark) colours: here they
            are the card's, so a light palette's list stays readable. */
         .nevis-switcher-list > row:not(:selected), .nevis-complete-list > row:not(:selected) {{ background: transparent; }}
         .nevis-switcher-list > row:hover:not(:selected), .nevis-complete-list > row:hover:not(:selected) {{ background: alpha({text}, 0.05); }}
         .nevis-switcher-list > row:selected, .nevis-complete-list > row:selected {{ background: alpha({accent}, 0.12); color: {text}; }}
         .nevis-prompt-title {{ font-weight: 600; padding: 2px 2px 0 2px; }}
         .nevis-prompt-error {{ color: {error}; }}
         .nevis-search-status {{ color: {muted}; font-size: 0.9em; padding: 0 4px; }}
         .nevis-search-place {{ font-size: 0.9em; }}
         .nevis-help {{ background: {surface}; color: {text}; border: 1px solid alpha({text}, 0.12); border-radius: 12px;
                       padding: 24px 28px; box-shadow: 0 6px 20px alpha(black, 0.35); }}
         .nevis-help-title {{ font-size: 1.2em; font-weight: 700; }}
         .nevis-help-section {{ color: {accent}; font-size: 0.8em; font-weight: 700; letter-spacing: 0.06em; text-transform: uppercase; }}
         /* Keys as keycaps. */
         .nevis-help-keys {{ color: {muted}; font-size: 0.8em; font-feature-settings: \"tnum\"; padding: 1px 7px; border-radius: 6px;
             background: alpha({text}, 0.08); border-bottom: 1px solid alpha({text}, 0.14); }}

         /* Tabs: chips in the frame, the one in front filled. */
         .nevis-tabbar {{ background: {surface}; padding: 6px 6px 0 6px; }}
         .nevis-tab {{ padding: 0 4px 0 10px; margin-right: 4px; border-radius: 8px; color: {muted};
             transition: color 120ms ease, background-color 120ms ease; }}
         .nevis-tab > label {{ padding: 5px 0; }}
         .nevis-tab:hover {{ color: {text}; background-color: alpha({text}, 0.05); }}
         .nevis-tab.current {{ color: {text}; background-color: alpha({text}, 0.12); }}
         .nevis-tab.current > .nevis-tab-title {{ font-weight: 600; }}
         .nevis-tab > .nevis-tab-number {{ margin-right: 6px; color: {muted}; font-size: 0.8em; }}
         .nevis-tab-close {{ min-width: 10px; padding: 0 6px; margin: 4px 0; color: transparent; border-radius: 6px;
             transition: color 120ms ease, background-color 120ms ease; }}
         .nevis-tab:hover > .nevis-tab-close, .nevis-tab.current > .nevis-tab-close {{ color: {muted}; }}
         .nevis-tab > .nevis-tab-close.unsaved {{ color: {accent}; font-size: 0.7em; }}
         .nevis-tab > .nevis-tab-close:hover {{ color: {text}; background-color: alpha({text}, 0.1); }}
         .nevis-tab-new, .nevis-sidebar-new {{ margin: 2px 0 2px 2px; padding: 0 10px; color: {muted}; border-radius: 8px;
             transition: color 120ms ease, background-color 120ms ease; }}
         .nevis-tab-new:hover, .nevis-sidebar-new:hover {{ color: {text}; background-color: alpha({text}, 0.08); }}
         .nevis-gear {{ margin: 8px 8px 2px 4px; padding: 0 8px; color: {muted}; border-radius: 8px;
             transition: color 120ms ease, background-color 120ms ease; }}
         .nevis-gear:hover {{ color: {text}; background-color: alpha({text}, 0.08); }}

         /* Settings: a window of its own, a side menu in the surface colour beside the pages. */
         .nevis-prefs, .nevis-prefs-pages {{ background: {background}; color: {text}; }}
         .nevis-prefs-nav {{ background: {surface}; min-width: 200px; border-right: 1px solid alpha({text}, 0.07); }}
         .nevis-prefs-nav-title {{ font-size: 1.05em; font-weight: 700; padding: 18px 12px 10px 20px; }}
         .nevis-prefs-list {{ background: transparent; }}
         .nevis-prefs-list > row {{ border-radius: 8px; margin: 1px 8px; padding: 7px 12px; color: {text}; outline: none; }}
         .nevis-prefs-list > row:hover:not(:selected) {{ background: alpha({text}, 0.05); }}
         .nevis-prefs-list > row:selected {{ background: alpha({accent}, 0.12); color: {text}; }}
         .nevis-prefs-list > row:selected label {{ font-weight: 600; }}
         .nevis-prefs-list > row:focus-visible {{ outline: 2px solid alpha({accent}, 0.7); outline-offset: -2px; }}
         .nevis-prefs-foot {{ border-radius: 0; min-height: 0; padding: 10px 20px; background: none; box-shadow: none; border: none;
             border-top: 1px solid alpha({text}, 0.07); color: {muted}; font-family: monospace; font-size: 0.8em; }}
         .nevis-prefs-foot:hover {{ background: alpha({text}, 0.05); color: {text}; }}
         .nevis-prefs-foot > label {{ color: inherit; }}
         .nevis-prefs-page {{ padding: 28px 36px 36px 36px; }}
         .nevis-prefs-close {{ margin: 10px 12px; min-width: 30px; min-height: 30px; padding: 0; border-radius: 8px; border: none;
             box-shadow: none; background: none; color: {muted}; font-size: 1.3em; }}
         .nevis-prefs-close:hover {{ background: alpha({text}, 0.08); color: {text}; }}
         .nevis-prefs-close > label {{ color: inherit; }}
         .nevis-prefs-heading {{ font-size: 1.6em; font-weight: 700; margin-bottom: 4px; }}
         .nevis-about {{ padding: 64px 32px 32px 32px; }}
         .nevis-about-name {{ font-size: 2.2em; font-weight: 800; letter-spacing: -0.02em; }}
         .nevis-about-by, .nevis-about-line {{ color: {muted}; }}
         .nevis-about-by link, .nevis-about-line link {{ color: inherit; }}
         .nevis-about-by link:hover, .nevis-about-line link:hover {{ color: {accent}; }}
         .nevis-about-version {{ color: {muted}; font-family: monospace; font-size: 0.8em; }}
         .nevis-about-credit {{ font-size: 0.8em; }}
         .nevis-settings-section {{ color: {muted}; font-size: 0.8em; font-weight: 700; letter-spacing: 0.08em; text-transform: uppercase; margin: 22px 0 8px 4px; }}
         .nevis-settings-group {{ background: alpha({text}, 0.035); border: 1px solid alpha({text}, 0.06); border-radius: 10px; }}
         .nevis-settings-row {{ padding: 10px 16px; min-height: 32px; border-bottom: 1px solid alpha({text}, 0.05); }}
         .nevis-settings-row:last-child, .nevis-settings-row.joined {{ border-bottom: none; }}
         .nevis-settings-about {{ color: {muted}; font-size: 0.8em; }}
         .nevis-settings-button, .nevis-stepper > button {{ min-height: 28px; padding: 0 10px; border-radius: 8px; border: none;
             box-shadow: none; background: alpha({text}, 0.06); color: {text}; font-size: 0.9em; font-weight: 500; }}
         .nevis-settings-button:hover, .nevis-stepper > button:hover {{ background: alpha({text}, 0.12); }}
         .nevis-stepper > button {{ font-weight: 600; }}
         .nevis-stepper-value {{ font-feature-settings: \"tnum\"; }}
         .nevis-switch {{ min-width: 36px; min-height: 20px; padding: 0; border-radius: 999px; border: 2px solid transparent;
             background: alpha({text}, 0.16); box-shadow: none; transition: background-color 150ms ease, border-color 150ms ease; }}
         .nevis-switch > .knob {{ min-width: 14px; min-height: 14px; margin-left: 3px; border-radius: 999px; background: {text};
             transition: all 150ms ease; }}
         .nevis-switch:hover {{ background: alpha({text}, 0.24); }}
         .nevis-switch:checked {{ background: {accent}; }}
         .nevis-switch:checked > .knob {{ min-width: 16px; min-height: 16px; margin-left: 16px; background: {on_accent}; }}
         /* Choices as one segmented control: a soft track, the chosen one raised. */
         .nevis-segments {{ background: alpha({text}, 0.06); border-radius: 10px; padding: 2px; }}
         .nevis-segments > button {{ padding: 3px 12px; min-height: 0; border-radius: 8px; border: none; box-shadow: none;
             background: none; color: {muted}; font-size: 0.9em; font-weight: 500; transition: color 120ms ease, background-color 120ms ease; }}
         .nevis-segments > button:hover {{ color: {text}; }}
         .nevis-segments > button:checked {{ background: alpha({text}, 0.14); color: {text}; font-weight: 600;
             box-shadow: 0 1px 2px alpha(black, 0.25); }}
         /* GTK stylesheets colour the label inside a checked button directly; it follows the button here. */
         .nevis-segments > button > label, .nevis-settings-button > label {{ color: inherit; font-weight: inherit; }}
         .nevis-themes, .nevis-themes > flowboxchild {{ background: none; padding: 0; }}
         .nevis-themes {{ padding: 0 10px 10px 10px; }}
         .nevis-theme-tile {{ padding: 6px 6px 8px 6px; border-radius: 10px; border: none; box-shadow: none; background: none;
             transition: background-color 120ms ease, box-shadow 120ms ease; }}
         .nevis-theme-tile:hover {{ background: alpha({text}, 0.06); }}
         .nevis-theme-tile.on {{ background: alpha({accent}, 0.14); box-shadow: inset 0 0 0 2px {accent}; }}
         .nevis-theme-name {{ color: {muted}; font-size: 0.8em; }}
         .nevis-theme-tile.on .nevis-theme-name {{ color: {text}; font-weight: 600; }}"
    )
}

/// GTK's own widgets in a built-in palette's colours: windows, menus, fields and buttons.
fn widget_css(c: &Colors) -> String {
    let h = |color: Rgb| color.to_hex();
    let (background, text, surface, accent) = (h(c.background), h(c.text), h(c.surface), h(c.accent));
    let field = h(c.background.mix(c.text, 0.06));
    let on_accent = h(if c.accent.contrast(Rgb::BLACK) > c.accent.contrast(Rgb::WHITE) { Rgb::BLACK } else { Rgb::WHITE });
    format!(
        "window, window.background, dialog {{ background-color: {background}; color: {text}; }}
         popover > contents, popover > arrow {{ background-color: {surface}; color: {text}; border-color: alpha({text}, 0.12); }}
         popover > contents {{ border-radius: 10px; }}
         popover modelbutton {{ border-radius: 6px; }}
         popover modelbutton:hover {{ background-color: alpha({text}, 0.08); }}
         entry, text {{ color: {text}; }}
         entry {{ background-color: {field}; border-color: alpha({text}, 0.12); border-radius: 8px; }}
         entry:focus-within {{ outline-color: alpha({accent}, 0.7); }}
         button {{ color: {text}; background-color: alpha({text}, 0.06); border-color: transparent; border-radius: 8px; }}
         button:hover {{ background-color: alpha({text}, 0.12); }}
         button.suggested-action, button.default {{ background-color: alpha({accent}, 0.85); color: {on_accent}; }}
         selection {{ background-color: alpha({accent}, 0.3); }}
         scrollbar slider {{ background-color: alpha({text}, 0.3); border-radius: 999px; }}"
    )
}

/// Changes the text size by `steps` points (a setting, shared by all windows); 0 resets it.
pub fn zoom(steps: f64) {
    settings::change(|s| s.text_size = if steps == 0.0 { settings::DEFAULT_TEXT_SIZE } else { s.text_size + steps });
}

/// The fonts, at the text size the settings say.
pub fn apply_font() {
    // Vazirmatn covers Persian, which the Latin fonts lack.
    let size = settings::get().text_size;
    let (code, body) = (fonts::mono_css(), fonts::body_css());
    let css = format!(
        "textview.nevis-editor {{ font-family: {code}, \"Vazirmatn\", monospace; font-size: {size}pt; }}
         textview.nevis-editor.nevis-live, textview.nevis-read {{ font-family: {body}, \"Vazirmatn\", sans-serif; font-size: {size}pt; }}"
    );
    FONT_CSS.with(|provider| provider.load_from_string(&css));
}
