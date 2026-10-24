//! Preferences: how nevis looks and behaves. Changed on the Settings page (Ctrl+,) or by
//! their shortcuts, kept in `~/.config/nevis/settings` as "name = value" lines.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::glib;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Long lines wrap in the source view.
    pub wrap: bool,
    pub line_numbers: bool,
    /// Enter continues Markdown lists and quotes.
    pub continue_lists: bool,
    /// Markdown files open rendered, rather than as source.
    pub open_rendered: bool,
    /// Files opened from outside get a window of their own, rather than a tab.
    pub outside_in_window: bool,
    /// The tab bar shows even with a single tab.
    pub tab_bar_always: bool,
    /// Files are saved a second after typing stops.
    pub autosave: bool,
    /// Misspelled words are underlined (libspelling, loaded when this is on).
    pub spell_check: bool,
    /// Text keeps to a readable column in the middle, rather than the window's full width.
    pub centered: bool,
    /// Markdown is edited in Live Preview (markup hidden except where the cursor is), rather
    /// than as plain source.
    pub live_preview: bool,
    /// Single line breaks join their lines, as on GitHub, rather than show (as in Obsidian).
    pub strict_line_breaks: bool,
    /// Text size, in points.
    pub text_size: f64,
}

pub const DEFAULT_TEXT_SIZE: f64 = 12.0;
const MIN_TEXT_SIZE: f64 = 7.0;
const MAX_TEXT_SIZE: f64 = 48.0;

impl Default for Settings {
    fn default() -> Self {
        Settings {
            wrap: true,
            line_numbers: true,
            continue_lists: true,
            open_rendered: true,
            outside_in_window: false,
            tab_bar_always: false,
            autosave: false,
            spell_check: false,
            centered: false,
            live_preview: true,
            strict_line_breaks: false,
            text_size: DEFAULT_TEXT_SIZE,
        }
    }
}

/// Called with the settings before and after each change.
type Listener = Rc<dyn Fn(Settings, Settings)>;

thread_local! {
    static CURRENT: RefCell<Option<Settings>> = const { RefCell::new(None) };
    static LISTENERS: RefCell<Vec<Listener>> = const { RefCell::new(Vec::new()) };
}

/// The settings in use.
pub fn get() -> Settings {
    CURRENT.with_borrow(|current| *current).unwrap_or_else(|| {
        let settings = parse(&std::fs::read_to_string(file()).unwrap_or_default());
        CURRENT.set(Some(settings));
        settings
    })
}

/// Changes the settings, keeps them for next time, and tells everyone listening.
pub fn change(edit: impl FnOnce(&mut Settings)) {
    let old = get();
    let mut new = old;
    edit(&mut new);
    new.text_size = new.text_size.clamp(MIN_TEXT_SIZE, MAX_TEXT_SIZE);
    if new == old {
        return;
    }
    CURRENT.set(Some(new));
    let path = file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, to_text(&new));
    let listeners = LISTENERS.with_borrow(|listeners| listeners.clone());
    for listener in listeners {
        listener(old, new);
    }
}

/// Runs `f` with the settings before and after every change.
pub fn on_change(f: impl Fn(Settings, Settings) + 'static) {
    LISTENERS.with_borrow_mut(|listeners| listeners.push(Rc::new(f)));
}

fn file() -> PathBuf {
    glib::user_config_dir().join("nevis").join("settings")
}

/// "name = value" lines; unknown names and values that don't read are left at their default.
fn parse(text: &str) -> Settings {
    let mut settings = Settings::default();
    for line in text.lines() {
        let Some((name, value)) = line.split_once('=') else { continue };
        let (name, value) = (name.trim(), value.trim());
        let flag = value.parse::<bool>().ok();
        match name {
            "wrap" => settings.wrap = flag.unwrap_or(settings.wrap),
            "line-numbers" => settings.line_numbers = flag.unwrap_or(settings.line_numbers),
            "continue-lists" => settings.continue_lists = flag.unwrap_or(settings.continue_lists),
            "open-rendered" => settings.open_rendered = flag.unwrap_or(settings.open_rendered),
            "outside-in-window" => settings.outside_in_window = flag.unwrap_or(settings.outside_in_window),
            "tab-bar-always" => settings.tab_bar_always = flag.unwrap_or(settings.tab_bar_always),
            "autosave" => settings.autosave = flag.unwrap_or(settings.autosave),
            "spell-check" => settings.spell_check = flag.unwrap_or(settings.spell_check),
            "centered" => settings.centered = flag.unwrap_or(settings.centered),
            "live-preview" => settings.live_preview = flag.unwrap_or(settings.live_preview),
            "strict-line-breaks" => settings.strict_line_breaks = flag.unwrap_or(settings.strict_line_breaks),
            "text-size" => {
                if let Ok(size) = value.parse::<f64>() {
                    settings.text_size = size.clamp(MIN_TEXT_SIZE, MAX_TEXT_SIZE);
                }
            }
            _ => {}
        }
    }
    settings
}

fn to_text(s: &Settings) -> String {
    format!(
        "wrap = {}\nline-numbers = {}\ncontinue-lists = {}\nopen-rendered = {}\noutside-in-window = {}\ntab-bar-always = {}\nautosave = {}\nspell-check = {}\ncentered = {}\nlive-preview = {}\nstrict-line-breaks = {}\ntext-size = {}\n",
        s.wrap, s.line_numbers, s.continue_lists, s.open_rendered, s.outside_in_window, s.tab_bar_always, s.autosave, s.spell_check,
        s.centered, s.live_preview, s.strict_line_breaks, s.text_size
    )
}
