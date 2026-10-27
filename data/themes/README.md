# nevis themes

Each `<id>.theme` file in `~/.config/nevis/themes/` is a theme. It shows up in Settings →
Appearance, among the light or the dark ones, depending on its background. A file with
the id of a built-in theme (`nord.theme`) takes that theme's place. While a theme from
this folder is in use, nevis repaints whenever its file changes.

The built-in themes are written the same way, so copy one to start from:
<https://github.com/Soheil7799/nevis/tree/main/data/themes>.

## Format

One `name = value` line each; a line starting with `#` is a comment.

```
name       = My Theme
background = #1a1b26
foreground = #c0caf5

# The rest is optional. Any you leave out is worked out from the accent.
surface    = #16161e
accent     = #7aa2f7
comment    = #565f89
red        = #f7768e
orange     = #ff9e64
yellow     = #e0af68
green      = #9ece6a
cyan       = #7dcfff
blue       = #7aa2f7
purple     = #bb9af7
```

- `background` and `foreground`: the page and its text. Required.
- `surface`: the sidebar, the tab bar, cards and code blocks.
- `accent`: what is chosen or focused, and links in some places.
- `comment`: quiet text (comments in code, the rendered view's quiet parts).
- The hues colour code, and some marks in notes.

nevis keeps text readable whatever the colours: one that is too faint on the background is
darkened or lightened until it reads.

## From matugen

A matugen template (`~/.config/matugen/templates/nevis.theme`) that names only what Material
You gives:

```
name       = Matugen
background = {{colors.surface.default.hex}}
foreground = {{colors.on_surface.default.hex}}
surface    = {{colors.surface_container.default.hex}}
accent     = {{colors.primary.default.hex}}
comment    = {{colors.outline.default.hex}}
red        = {{colors.error.default.hex}}
```

and in `~/.config/matugen/config.toml`:

```
[templates.nevis]
input_path = "~/.config/matugen/templates/nevis.theme"
output_path = "~/.config/nevis/themes/matugen.theme"
```

If your GTK theme already follows the wallpaper (noctalia, matugen's GTK template), the
System theme follows it too, without any of this.
