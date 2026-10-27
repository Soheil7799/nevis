//! Colour palettes: the well-known ones built in, or the GTK theme's own (System), and every
//! colour nevis paints with, worked out from a palette and kept readable.

use crate::color::Rgb;
use crate::gtkcolors::Named;

/// A palette's own colours.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub id: String,
    pub name: String,
    pub background: Rgb,
    pub foreground: Rgb,
    /// Sidebars, cards, code blocks.
    pub surface: Rgb,
    pub accent: Rgb,
    /// Comments and other quiet text.
    pub comment: Rgb,
    pub red: Rgb,
    pub orange: Rgb,
    pub yellow: Rgb,
    pub green: Rgb,
    pub cyan: Rgb,
    pub blue: Rgb,
    pub purple: Rgb,
}

/// Built in: each an id and a theme file, in the format of the user's own (see `read`), in
/// menu order.
const BUILT_IN: &[(&str, &str)] = &[
    ("adwaita-dark", include_str!("../data/themes/adwaita-dark.theme")),
    ("adwaita-light", include_str!("../data/themes/adwaita-light.theme")),
    ("ayu-dark", include_str!("../data/themes/ayu-dark.theme")),
    ("ayu-light", include_str!("../data/themes/ayu-light.theme")),
    ("ayu-mirage", include_str!("../data/themes/ayu-mirage.theme")),
    ("catppuccin-latte", include_str!("../data/themes/catppuccin-latte.theme")),
    ("catppuccin-mocha", include_str!("../data/themes/catppuccin-mocha.theme")),
    ("dracula", include_str!("../data/themes/dracula.theme")),
    ("everforest", include_str!("../data/themes/everforest.theme")),
    ("github-dark", include_str!("../data/themes/github-dark.theme")),
    ("github-light", include_str!("../data/themes/github-light.theme")),
    ("gruvbox-dark", include_str!("../data/themes/gruvbox-dark.theme")),
    ("gruvbox-light", include_str!("../data/themes/gruvbox-light.theme")),
    ("kanagawa", include_str!("../data/themes/kanagawa.theme")),
    ("nord", include_str!("../data/themes/nord.theme")),
    ("one-dark", include_str!("../data/themes/one-dark.theme")),
    ("rose-pine", include_str!("../data/themes/rose-pine.theme")),
    ("rose-pine-dawn", include_str!("../data/themes/rose-pine-dawn.theme")),
    ("solarized-dark", include_str!("../data/themes/solarized-dark.theme")),
    ("solarized-light", include_str!("../data/themes/solarized-light.theme")),
    ("tokyo-night", include_str!("../data/themes/tokyo-night.theme")),
];

/// The built-in palettes, in menu order.
pub fn built_in() -> Vec<Palette> {
    BUILT_IN.iter().filter_map(|(id, text)| read(id, text)).collect()
}

/// A theme file: "name = value" lines, "#" starting a comment line. `background` and
/// `foreground` it must have; `name`, `surface`, `accent`, `comment` and the hues (`red`,
/// `orange`, `yellow`, `green`, `cyan`, `blue`, `purple`) it may, the rest worked out as for a
/// GTK theme. Nothing without a background and a foreground.
pub fn read(id: &str, text: &str) -> Option<Palette> {
    let mut name = None;
    let mut colors: Vec<(&str, Rgb)> = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.starts_with('#')) {
        let Some((key, value)) = line.split_once('=') else { continue };
        let (key, value) = (key.trim(), value.trim());
        if key == "name" {
            name = Some(value);
        } else if let Some(color) = Rgb::hex(value) {
            colors.push((key, color));
        }
    }
    let given = |key: &str| colors.iter().find(|(k, _)| *k == key).map(|(_, color)| *color);
    Some(complete(id, name.filter(|name| !name.is_empty()).unwrap_or(id), given("background")?, given("foreground")?, given))
}

/// A palette from its background, its foreground and whichever other colours `given` names
/// (as a theme file does); the ones it doesn't, worked out from those.
fn complete(id: &str, name: &str, background: Rgb, foreground: Rgb, given: impl Fn(&str) -> Option<Rgb>) -> Palette {
    let background = Rgb { a: 1.0, ..background };
    // A see-through colour counts as it shows over the background.
    let opaque = |color: Rgb| color.over(background);
    let foreground = opaque(foreground);
    let accent = given("accent").map_or(Rgb::from_oklch(0.65, 0.12, 250.0), opaque);
    // The hues not given, from the accent: clearly coloured, and a little deeper than the pale
    // accents of Material You, so that code reads apart.
    let dark = background.luminance() < foreground.luminance();
    let (lightness, chroma, _) = accent.oklch();
    let lightness = if dark { lightness.clamp(0.70, 0.78) } else { lightness.clamp(0.45, 0.55) };
    let hue = |key: &str, degrees: f64| given(key).map_or_else(|| Rgb::from_oklch(lightness, chroma.clamp(0.12, 0.16), degrees), opaque);
    Palette {
        id: id.to_owned(),
        name: name.to_owned(),
        background,
        foreground,
        surface: given("surface").map_or(background.mix(foreground, 0.05), opaque),
        accent,
        comment: given("comment").map_or(foreground.mix(background, 0.45), opaque),
        red: hue("red", 25.0),
        orange: hue("orange", 60.0),
        yellow: hue("yellow", 95.0),
        green: hue("green", 145.0),
        cyan: hue("cyan", 195.0),
        blue: hue("blue", 255.0),
        purple: hue("purple", 310.0),
    }
}

/// The System palette: the GTK theme's own colours, from the names libadwaita and older
/// themes give them. Nothing without a background and a foreground.
pub fn from_gtk(named: &Named) -> Option<Palette> {
    let first = |names: &[&str]| names.iter().find_map(|name| named.color(name));
    let background = first(&["view_bg_color", "window_bg_color", "theme_base_color", "theme_bg_color"])?;
    let foreground = first(&["view_fg_color", "window_fg_color", "theme_text_color", "theme_fg_color"])?;
    // GTK names an accent, a surface and an error colour; nevis works out the other hues.
    let given = |key: &str| match key {
        "accent" => first(&["accent_color", "accent_bg_color", "theme_selected_bg_color"]),
        "surface" => first(&["card_bg_color", "sidebar_bg_color", "popover_bg_color", "headerbar_bg_color"]),
        "red" => first(&["error_color", "error_bg_color", "destructive_color", "destructive_bg_color"]),
        _ => None,
    };
    Some(complete("system", "System", background, foreground, given))
}

/// Every colour nevis paints with.
#[derive(Clone, Debug)]
pub struct Colors {
    pub dark: bool,
    pub background: Rgb,
    pub text: Rgb,
    pub surface: Rgb,
    pub accent: Rgb,
    pub heading: Rgb,
    pub link: Rgb,
    pub code: Rgb,
    pub code_background: Rgb,
    pub quote: Rgb,
    pub quote_background: Rgb,
    pub dim: Rgb,
    pub rule: Rgb,
    pub bullet: Rgb,
    /// Markup characters in the source view: there, but quiet.
    pub marker: Rgb,
    pub selection: Rgb,
    pub current_line: Rgb,
    pub search_match: Rgb,
    pub border: Rgb,
    pub error: Rgb,
    pub keyword: Rgb,
    pub function: Rgb,
    pub kind: Rgb,
    pub string: Rgb,
    pub number: Rgb,
    pub special: Rgb,
    pub added: Rgb,
    pub removed: Rgb,
    pub changed: Rgb,
}

impl Colors {
    pub fn from(palette: &Palette) -> Colors {
        let p = palette;
        let background = Rgb { a: 1.0, ..p.background };
        let on = |color: Rgb, ratio: f64| color.readable_on(background, ratio);
        let mut text = on(p.foreground, 4.5);
        // Selections tint the background blue, never so much that text gets hard to read; on
        // palettes whose text barely stands out (Solarized), the text gives a little instead.
        let tint = [0.32, 0.24, 0.16, 0.1].into_iter().map(|amount| background.mix(p.blue, amount));
        let selection = match tint.clone().find(|selection| text.contrast(*selection) >= 4.5) {
            Some(selection) => selection,
            None => {
                let faintest = background.mix(p.blue, 0.1);
                text = text.readable_on(faintest, 4.5);
                faintest
            }
        };
        let code_background = p.surface;
        let quote_background = background.mix(text, 0.05);
        Colors {
            dark: background.luminance() < text.luminance(),
            background,
            text,
            surface: p.surface,
            accent: on(p.accent, 3.0),
            // Structure is told by size and weight; the accent is kept for state, a tint of it here.
            heading: on(text.mix(p.accent, 0.15), 4.5),
            link: on(p.blue, 4.5),
            code: p.cyan.readable_on(code_background, 4.5),
            code_background,
            quote: text.mix(background, 0.15).readable_on(quote_background, 4.5),
            quote_background,
            dim: on(p.comment, 3.0),
            rule: text.mix(background, 0.75),
            bullet: on(p.orange, 3.0),
            marker: on(text.mix(background, 0.6), 2.0),
            selection,
            current_line: background.mix(text, 0.04),
            search_match: background.mix(p.purple, 0.35),
            border: background.mix(text, 0.18),
            error: on(p.red, 3.0),
            keyword: on(p.purple, 3.0),
            function: on(p.blue, 3.0),
            kind: on(p.yellow, 3.0),
            string: on(p.green, 3.0),
            number: on(p.orange, 3.0),
            special: on(p.cyan, 3.0),
            added: on(p.green, 3.0),
            removed: on(p.red, 3.0),
            changed: on(p.orange, 3.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every role readable where it is painted: text and links like body text, the rest
    /// like large or bold text, markup quietly.
    fn assert_readable(colors: &Colors, name: &str) {
        let checks = [
            ("text", colors.text, colors.background, 4.5),
            ("link", colors.link, colors.background, 4.5),
            ("code", colors.code, colors.code_background, 4.5),
            ("quote", colors.quote, colors.quote_background, 4.5),
            ("text on selection", colors.text, colors.selection, 4.5),
            ("heading", colors.heading, colors.background, 3.0),
            ("dim", colors.dim, colors.background, 3.0),
            ("bullet", colors.bullet, colors.background, 3.0),
            ("error", colors.error, colors.background, 3.0),
            ("keyword", colors.keyword, colors.background, 3.0),
            ("string", colors.string, colors.background, 3.0),
            ("marker", colors.marker, colors.background, 2.0),
        ];
        for (role, color, on, least) in checks {
            let ratio = color.contrast(on);
            assert!(ratio >= least, "{name}: {role} {} on {} is {ratio:.2}", color.to_hex(), on.to_hex());
        }
    }

    fn gtk(css: &str) -> Named {
        Named::read(css, &mut |_| None)
    }

    #[test]
    fn the_gtk_theme_makes_the_system_palette() {
        // As noctalia writes it from the wallpaper.
        let named = gtk(
            "@define-color accent_color @accent_bg_color;
             @define-color accent_bg_color #9fcafd;
             @define-color error_bg_color #ffb4ab;
             @define-color window_bg_color #101418;
             @define-color window_fg_color #e1e2e8;
             @define-color view_bg_color #101418;
             @define-color view_fg_color #e1e2e8;
             @define-color card_bg_color #1d2024;",
        );
        let palette = from_gtk(&named).unwrap();
        let picked = [palette.background, palette.foreground, palette.accent, palette.surface, palette.red].map(Rgb::to_hex);
        assert_eq!(picked, ["#101418", "#e1e2e8", "#9fcafd", "#1d2024", "#ffb4ab"]);
        // The hues GTK doesn't name are clearly coloured, a little deeper than a pale accent,
        // so that code reads apart: on a dark background, lightness 0.70–0.80.
        for hue in [palette.green, palette.blue, palette.purple, palette.orange] {
            let (l, c, _) = hue.oklch();
            assert!((0.70..=0.80).contains(&l) && c >= 0.10, "{} is {l:.2} light, {c:.3} coloured", hue.to_hex());
        }
        let colors = Colors::from(&palette);
        assert!(colors.dark);
        assert_readable(&colors, "System");
    }

    #[test]
    fn a_light_gtk_theme_makes_a_light_palette_and_too_little_makes_none() {
        let light = gtk(
            "@define-color window_bg_color #fafafa;
             @define-color window_fg_color rgba(0, 0, 0, 0.8);
             @define-color accent_bg_color #3584e4;",
        );
        let palette = from_gtk(&light).unwrap();
        // A see-through colour counts as it shows over the background.
        assert_eq!(palette.foreground.to_hex(), "#323232");
        let colors = Colors::from(&palette);
        assert!(!colors.dark);
        assert_readable(&colors, "light GTK");
        assert_eq!(from_gtk(&gtk("@define-color accent_bg_color #3584e4;")), None);
    }

    #[test]
    fn a_theme_file_needs_only_a_background_and_a_foreground() {
        let palette = read("mine", "# a comment\nbackground = #101418\nforeground = #e0e2e8\naccent=#a0c4ff\nbogus = #123456\n").unwrap();
        assert_eq!((palette.id.as_str(), palette.name.as_str()), ("mine", "mine"));
        assert_eq!(palette.accent.to_hex(), "#a0c4ff");
        assert_readable(&Colors::from(&palette), "a theme with the hues worked out");
        let named = read("mine", "name = Mine\nbackground = #ffffff\nforeground = #202020\nred = #c01c28\n").unwrap();
        assert_eq!((named.name.as_str(), named.red.to_hex().as_str()), ("Mine", "#c01c28"));
        assert_eq!(read("mine", "background = #ffffff\n"), None);
        assert_eq!(read("mine", "background = #ffffff\nforeground = white\n"), None);
    }

    #[test]
    fn every_built_in_theme_file_reads() {
        assert_eq!(built_in().len(), BUILT_IN.len());
        let nord = built_in().into_iter().find(|p| p.id == "nord").unwrap();
        assert_eq!((nord.name.as_str(), nord.surface.to_hex().as_str(), nord.purple.to_hex().as_str()), ("Nord", "#3b4252", "#b48ead"));
    }

    #[test]
    fn every_built_in_palette_paints_readable_colours() {
        let palettes = built_in();
        assert!(!palettes.is_empty());
        for palette in &palettes {
            assert_readable(&Colors::from(palette), &palette.name);
        }
    }
}
