//! The keyboard shortcut cheat sheet (Ctrl+? or F1), drawn over the window.
//!
//! Key labels come from the app's real bindings, so the sheet can't drift from them.

use gtk::prelude::*;

struct Row {
    what: &'static str,
    /// Action whose bindings are listed first ("" for none).
    action: &'static str,
    /// Keys GTK or the find bar handle themselves, listed after the action's.
    also: &'static str,
}

const fn row(what: &'static str, action: &'static str, also: &'static str) -> Row {
    Row { what, action, also }
}

const LEFT: &[(&str, &[Row])] = &[
    ("File", &[
        row("New note (named, in a folder window)", "win.new-note", ""),
        row("New note from a template", "win.new-from-template", ""),
        row("New tab", "win.new-tab", ""),
        row("New window", "app.new-window", ""),
        row("Open a file", "win.open", ""),
        row("Open a folder", "win.open-folder", ""),
        row("Go to a file, heading or #tag", "win.quick-open", ""),
        row("Search in the folder", "win.search", ""),
        row("Save", "win.save", ""),
        row("Save as", "win.save-as", ""),
        row("Export as PDF", "win.export-pdf", ""),
        row("Export as HTML", "win.export-html", ""),
        row("Print", "win.print", ""),
        row("Close tab", "win.close", ""),
        row("Reopen closed tab", "win.reopen-tab", ""),
        row("Note history: go back to an earlier version", "win.history", ""),
        row("Close window", "win.close-window", ""),
        row("Quit, keeping every tab", "app.quit", ""),
    ]),
    ("Tabs", &[
        row("Next / previous tab", "win.next-tab", "Ctrl+Shift+Tab"),
        row("Tab 1 … 8, last tab", "win.tab-1", "… Alt+9"),
        row("Move tab left / right", "win.move-tab-left", "Ctrl+Shift+Page Down"),
    ]),
    ("Find", &[
        row("Find", "win.find", ""),
        row("Replace", "win.replace", ""),
        row("Next match", "win.find-next", "Enter"),
        row("Previous match", "win.find-prev", "Shift+Enter"),
        row("Close the find bar", "", "Esc"),
    ]),
    ("Help", &[
        row("This cheat sheet", "win.shortcuts", ""),
        row("Command palette: run any command by name", "win.palette", ""),
        row("Menu", "", "Right-click / Shift+F10"),
    ]),
];

const RIGHT: &[(&str, &[Row])] = &[
    ("View", &[
        row("Rendered view ⇄ source", "win.reading", ""),
        row("Source and rendered view side by side", "win.side-by-side", ""),
        row("Folder sidebar", "win.sidebar", ""),
        row("Focus mode: only the paragraph you're writing", "win.focus", ""),
        row("Settings (theme, text size…)", "win.settings", ""),
        row("About nevis", "win.about", ""),
        row("Line numbers", "win.line-numbers", ""),
        row("Word wrap", "win.wrap", ""),
        row("Larger text", "win.zoom-in", "Ctrl+scroll"),
        row("Smaller text", "win.zoom-out", ""),
        row("Default text size", "win.zoom-reset", ""),
    ]),
    ("Notes", &[
        row("Go to a heading", "win.outline", ""),
        row("Today's note (in a folder window)", "win.daily", ""),
        row("Make a link (or paste an address over words)", "win.link", ""),
        row("Paste a screenshot (saved in attachments/)", "", "Ctrl+V"),
        row("Complete [[links]], #tags, > [!callouts]", "", "Enter / Tab"),
        row("Next / previous table cell, new row", "", "Tab / Shift+Tab / Enter"),
    ]),
    ("Editing", &[
        row("Undo / redo", "", "Ctrl+Z / Shift+Ctrl+Z"),
        row("Select all", "", "Ctrl+A"),
        row("Cut / copy / paste", "", "Ctrl+X / C / V"),
        row("Emoji", "", "Ctrl+."),
    ]),
    ("Rendered view", &[
        row("Follow a link", "", "Click"),
        row("Tick a checkbox", "", "Click"),
        row("Search for a #tag", "", "Click"),
        row("Fold or unfold a callout", "", "Click its title"),
        row("Copy a code block", "", "Click"),
        row("Edit the source", "", "Double-click"),
    ]),
];

/// Builds the sheet: a dimmed backdrop with a centered card. Clicking it closes it.
pub fn build(app: &gtk::Application, close: impl Fn() + 'static) -> gtk::Widget {
    let title = gtk::Label::builder().label("Keyboard shortcuts").css_classes(["nevis-help-title"]).build();
    let columns = gtk::Box::new(gtk::Orientation::Horizontal, 32);
    columns.append(&column(app, LEFT));
    columns.append(&column(app, RIGHT));
    let hint = gtk::Label::builder().label("Esc or click to close").css_classes(["dim-label"]).build();

    let card = gtk::Box::new(gtk::Orientation::Vertical, 16);
    card.add_css_class("nevis-help");
    card.append(&title);
    card.append(&columns);
    card.append(&hint);

    // Scrolls when the window is smaller than the card.
    let scroller = gtk::ScrolledWindow::builder()
        .child(&card)
        .propagate_natural_width(true)
        .propagate_natural_height(true)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .margin_top(16)
        .margin_bottom(16)
        .build();

    let backdrop = gtk::Box::new(gtk::Orientation::Vertical, 0);
    backdrop.add_css_class("nevis-help-backdrop");
    backdrop.set_hexpand(true);
    backdrop.set_vexpand(true);
    backdrop.append(&scroller);
    let click = gtk::GestureClick::new();
    click.connect_released(move |_, _, _, _| close());
    backdrop.add_controller(click);
    backdrop.set_visible(false);
    backdrop.upcast()
}

/// A command the palette offers: what it does, its action, and its keys.
pub struct Command {
    pub what: String,
    /// A detailed action name: "win.open", or with its argument, "win.switch-to('/a.md')".
    pub action: String,
    pub keys: String,
}

/// What the palette calls the commands the sheet lists in pairs ("Next / previous tab" runs
/// the next tab), and the other half of each pair.
const HALVES: &[(&str, &str)] = &[("win.next-tab", "Next tab"), ("win.move-tab-left", "Move tab left"), ("win.tab-1", "First tab")];
/// Commands only the palette offers: too many for the sheet, and without keys.
const PALETTE_ONLY: &[Row] = &[
    row("Table: add a column", "win.table-add-column", ""),
    row("Table: delete the column", "win.table-delete-column", ""),
    row("Table: align the column left", "win.table-align-left", ""),
    row("Table: center the column", "win.table-align-center", ""),
    row("Table: align the column right", "win.table-align-right", ""),
];
const OTHER_HALVES: &[Row] = &[row("Previous tab", "win.prev-tab", ""), row("Move tab right", "win.move-tab-right", ""), row("Last tab", "win.tab-9", "")];

/// Every command with an action, in the cheat sheet's order, with its keys (`also` aside:
/// those are handled elsewhere, as in the find bar).
pub fn commands(app: &gtk::Application) -> Vec<Command> {
    LEFT.iter()
        .chain(RIGHT)
        .flat_map(|(_, rows)| rows.iter())
        .chain(OTHER_HALVES)
        .chain(PALETTE_ONLY)
        .filter(|row| !row.action.is_empty())
        .map(|row| {
            let what = HALVES.iter().find(|(action, _)| *action == row.action).map_or(row.what, |(_, what)| what);
            Command { what: what.to_owned(), action: row.action.to_owned(), keys: keys(app, &Row { also: "", ..*row }) }
        })
        .collect()
}

fn column(app: &gtk::Application, sections: &[(&str, &[Row])]) -> gtk::Grid {
    let grid = gtk::Grid::builder().row_spacing(6).column_spacing(24).valign(gtk::Align::Start).build();
    let mut line = 0;
    for (name, rows) in sections {
        let header = gtk::Label::builder().label(*name).xalign(0.0).css_classes(["nevis-help-section"]).build();
        if line > 0 {
            header.set_margin_top(12);
        }
        grid.attach(&header, 0, line, 2, 1);
        line += 1;
        for row in *rows {
            let what = gtk::Label::builder().label(row.what).xalign(0.0).build();
            // A keycap as wide as its keys; none for a command without any.
            let keys = keys(app, row);
            let keys = gtk::Label::builder()
                .visible(!keys.is_empty())
                .label(keys)
                .halign(gtk::Align::Start)
                .valign(gtk::Align::Center)
                .css_classes(["nevis-help-keys"])
                .build();
            grid.attach(&what, 0, line, 1, 1);
            grid.attach(&keys, 1, line, 1, 1);
            line += 1;
        }
    }
    grid
}

/// "F3 / Enter": the action's main binding, then the keys handled elsewhere.
fn keys(app: &gtk::Application, row: &Row) -> String {
    let mut labels: Vec<String> = if row.action.is_empty() {
        Vec::new()
    } else {
        app.accels_for_action(row.action)
            .iter()
            .filter(|accel| !accel.contains("KP_"))
            .filter_map(|accel| gtk::accelerator_parse(accel.as_str()))
            .map(|(key, mods)| gtk::accelerator_get_label(key, mods).to_string())
            .take(1)
            .collect()
    };
    if !row.also.is_empty() {
        labels.push(row.also.to_owned());
    }
    labels.join(" / ")
}
