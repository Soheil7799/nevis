# nevis

A simple, extremely fast GUI text editor, written in Rust with GTK 4. Inspired by Obsidian,
Mousepad, Notepad and micro.

I wanted something to edit text without opening a heavy application like VS Code or Zed.

## Features

- Tabs, with unsaved notes kept
- Markdown with Live Preview, as in Obsidian
- `[[links]]`, callouts, tags, embeds, backlinks
- Math and Mermaid diagrams
- Folder sidebar, quick open, search
- Persian and right-to-left text
- Syntax highlighting
- Themes following your GTK colours, 21 built in, and your own (or matugen's) in
  `~/.config/nevis/themes` ([format](data/themes/README.md))

## Build and install

nevis runs on Linux with GTK 4.14 or later and GtkSourceView 5. Spell checking also needs
libspelling and a Hunspell dictionary.

From a release: download `nevis-<version>-x86_64-linux.tar.gz` from
[Releases](https://github.com/Soheil7799/nevis/releases), unpack it and run `./install.sh` in
it. It is built on Ubuntu 24.04, so it runs there and on anything newer (Debian 13, Fedora 40,
Arch…).

From source, with Rust installed: run `./install.sh` in a checkout; it builds nevis first.
On Arch, `cd packaging/arch && makepkg -si` builds a package instead.

`install.sh` puts `nevis` and `nevis-app` in `~/.local/bin`, adds a desktop entry and a
systemd user service that keeps nevis ready, and makes nevis the default app for `.txt` and
`.md` files (`NEVIS_DEFAULT=0 ./install.sh` leaves those alone). After installing a newer
version, `systemctl --user restart nevis.service` switches to it; your tabs come back.

## Use

```
nevis notes.md      # open (or create) a file
nevis a.md b.md     # several files, a tab each
nevis ~/Notes       # a folder, its notes in a sidebar
nevis               # nevis as you left it
```

Ctrl+E switches a Markdown note between the rendered view and its source. Ctrl+Shift+P finds
any command by name, F1 lists the keyboard shortcuts, and Ctrl+, (or the gear at the end of the
tab bar) opens the settings: appearance, writing, windows and tabs, and About.
