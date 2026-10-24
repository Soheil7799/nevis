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

/// Built in: id, name, and background, foreground, surface, accent, comment, red, orange,
/// yellow, green, cyan, blue, purple.
#[rustfmt::skip]
const BUILT_IN: &[(&str, &str, [&str; 12])] = &[
    ("adwaita-dark", "Adwaita Dark", ["#1e1e1e", "#ffffff", "#303030", "#78aeed", "#9a9996", "#ff7b63", "#ffa348", "#f8e45c", "#8ff0a4", "#93ddc2", "#99c1f1", "#dc8add"]),
    ("adwaita-light", "Adwaita Light", ["#ffffff", "#323232", "#f6f5f4", "#3584e4", "#77767b", "#c01c28", "#c64600", "#9c6e03", "#26a269", "#1f8a8a", "#1c71d8", "#813d9c"]),
    ("ayu-mirage", "Ayu Mirage", ["#1f2430", "#cccac2", "#1a1f29", "#ffcc66", "#707a8c", "#f28779", "#ffad66", "#ffd173", "#bae67e", "#95e6cb", "#73d0ff", "#dfbfff"]),
    ("ayu-dark", "Ayu Dark", ["#0d1017", "#bfbdb6", "#0b0e14", "#e6b450", "#636a72", "#f07178", "#ff8f40", "#ffb454", "#aad94c", "#95e6cb", "#59c2ff", "#d2a6ff"]),
    ("ayu-light", "Ayu Light", ["#fcfcfc", "#5c6166", "#f3f4f5", "#ffaa33", "#8a9199", "#f07171", "#fa8d3e", "#f2ae49", "#86b300", "#4cbf99", "#399ee6", "#a37acc"]),
    ("catppuccin-mocha", "Catppuccin Mocha", ["#1e1e2e", "#cdd6f4", "#181825", "#cba6f7", "#7f849c", "#f38ba8", "#fab387", "#f9e2af", "#a6e3a1", "#94e2d5", "#89b4fa", "#cba6f7"]),
    ("catppuccin-latte", "Catppuccin Latte", ["#eff1f5", "#4c4f69", "#e6e9ef", "#8839ef", "#8c8fa1", "#d20f39", "#fe640b", "#df8e1d", "#40a02b", "#179299", "#1e66f5", "#8839ef"]),
    ("dracula", "Dracula", ["#282a36", "#f8f8f2", "#21222c", "#bd93f9", "#6272a4", "#ff5555", "#ffb86c", "#f1fa8c", "#50fa7b", "#8be9fd", "#8be9fd", "#ff79c6"]),
    ("everforest", "Everforest", ["#2d353b", "#d3c6aa", "#343f44", "#a7c080", "#859289", "#e67e80", "#e69875", "#dbbc7f", "#a7c080", "#83c092", "#7fbbb3", "#d699b6"]),
    ("github-dark", "GitHub Dark", ["#0d1117", "#e6edf3", "#161b22", "#58a6ff", "#8b949e", "#ff7b72", "#ffa657", "#e3b341", "#7ee787", "#a5d6ff", "#79c0ff", "#d2a8ff"]),
    ("github-light", "GitHub Light", ["#ffffff", "#1f2328", "#f6f8fa", "#0969da", "#6e7781", "#cf222e", "#bc4c00", "#9a6700", "#116329", "#0a3069", "#0550ae", "#8250df"]),
    ("gruvbox-dark", "Gruvbox Dark", ["#282828", "#ebdbb2", "#32302f", "#fabd2f", "#928374", "#fb4934", "#fe8019", "#fabd2f", "#b8bb26", "#8ec07c", "#83a598", "#d3869b"]),
    ("gruvbox-light", "Gruvbox Light", ["#fbf1c7", "#3c3836", "#f2e5bc", "#af3a03", "#928374", "#9d0006", "#af3a03", "#b57614", "#79740e", "#427b58", "#076678", "#8f3f71"]),
    ("kanagawa", "Kanagawa", ["#1f1f28", "#dcd7ba", "#2a2a37", "#7e9cd8", "#727169", "#e46876", "#ffa066", "#e6c384", "#98bb6c", "#7aa89f", "#7e9cd8", "#957fb8"]),
    ("nord", "Nord", ["#2e3440", "#d8dee9", "#3b4252", "#88c0d0", "#616e88", "#bf616a", "#d08770", "#ebcb8b", "#a3be8c", "#8fbcbb", "#81a1c1", "#b48ead"]),
    ("one-dark", "One Dark", ["#282c34", "#abb2bf", "#21252b", "#61afef", "#5c6370", "#e06c75", "#d19a66", "#e5c07b", "#98c379", "#56b6c2", "#61afef", "#c678dd"]),
    ("rose-pine", "Rosé Pine", ["#191724", "#e0def4", "#1f1d2e", "#ebbcba", "#6e6a86", "#eb6f92", "#ebbcba", "#f6c177", "#9ccfd8", "#9ccfd8", "#31748f", "#c4a7e7"]),
    ("rose-pine-dawn", "Rosé Pine Dawn", ["#faf4ed", "#575279", "#fffaf3", "#d7827e", "#9893a5", "#b4637a", "#d7827e", "#ea9d34", "#56949f", "#56949f", "#286983", "#907aa9"]),
    ("solarized-dark", "Solarized Dark", ["#002b36", "#839496", "#073642", "#b58900", "#586e75", "#dc322f", "#cb4b16", "#b58900", "#859900", "#2aa198", "#268bd2", "#6c71c4"]),
    ("solarized-light", "Solarized Light", ["#fdf6e3", "#657b83", "#eee8d5", "#b58900", "#93a1a1", "#dc322f", "#cb4b16", "#b58900", "#859900", "#2aa198", "#268bd2", "#6c71c4"]),
    ("tokyo-night", "Tokyo Night", ["#1a1b26", "#c0caf5", "#16161e", "#7aa2f7", "#565f89", "#f7768e", "#ff9e64", "#e0af68", "#9ece6a", "#7dcfff", "#7aa2f7", "#bb9af7"]),
];

/// The built-in palettes, in menu order.
pub fn built_in() -> Vec<Palette> {
    BUILT_IN
        .iter()
        .map(|(id, name, hex)| {
            let c: Vec<Rgb> = hex.iter().map(|h| Rgb::hex(h).unwrap_or(Rgb::BLACK)).collect();
            Palette {
                id: (*id).to_owned(),
                name: (*name).to_owned(),
                background: c[0],
                foreground: c[1],
                surface: c[2],
                accent: c[3],
                comment: c[4],
                red: c[5],
                orange: c[6],
                yellow: c[7],
                green: c[8],
                cyan: c[9],
                blue: c[10],
                purple: c[11],
            }
        })
        .collect()
}

/// The System palette: the GTK theme's own colours, from the names libadwaita and older
/// themes give them. Nothing without a background and a foreground.
pub fn from_gtk(named: &Named) -> Option<Palette> {
    let first = |names: &[&str]| names.iter().find_map(|name| named.color(name));
    let background = Rgb { a: 1.0, ..first(&["view_bg_color", "window_bg_color", "theme_base_color", "theme_bg_color"])? };
    let opaque = |color: Rgb| color.over(background);
    let foreground = opaque(first(&["view_fg_color", "window_fg_color", "theme_text_color", "theme_fg_color"])?);
    let accent = first(&["accent_color", "accent_bg_color", "theme_selected_bg_color"]).map_or(Rgb::from_oklch(0.65, 0.12, 250.0), opaque);
    let surface = first(&["card_bg_color", "sidebar_bg_color", "popover_bg_color", "headerbar_bg_color"])
        .map_or(background.mix(foreground, 0.05), opaque);
    // The hues GTK has no names for, from its accent: clearly coloured, and a little deeper
    // than the pale accents of Material You, so that code reads apart.
    let dark = background.luminance() < foreground.luminance();
    let (lightness, chroma, _) = accent.oklch();
    let lightness = if dark { lightness.clamp(0.70, 0.78) } else { lightness.clamp(0.45, 0.55) };
    let hue = |degrees: f64| Rgb::from_oklch(lightness, chroma.clamp(0.12, 0.16), degrees);
    let red = first(&["error_color", "error_bg_color", "destructive_color", "destructive_bg_color"]).map_or(hue(25.0), opaque);
    Some(Palette {
        id: "system".to_owned(),
        name: "System".to_owned(),
        background,
        foreground,
        surface,
        accent,
        comment: foreground.mix(background, 0.45),
        red,
        orange: hue(60.0),
        yellow: hue(95.0),
        green: hue(145.0),
        cyan: hue(195.0),
        blue: hue(255.0),
        purple: hue(310.0),
    })
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
    fn every_built_in_palette_paints_readable_colours() {
        let palettes = built_in();
        assert!(!palettes.is_empty());
        for palette in &palettes {
            assert_readable(&Colors::from(palette), &palette.name);
        }
    }
}
