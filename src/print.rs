//! Printing, and exporting as a PDF: the note laid out on a view of its own, out of sight
//! beside the tab, in colours for paper (a Markdown note as the rendered view shows it, any
//! other text as it is written), then drawn page by page, each page ending between rows of
//! text.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk::{gdk, gio, glib, prelude::*};
use sourceview::prelude::*;

use crate::paper::Paper;
use crate::{render, theme};

/// CSS pixels (1/96 inch) to a point (1/72 inch).
const PX_PER_PT: f64 = 96.0 / 72.0;
/// Page margins, in millimetres.
const MARGIN_MM: f64 = 16.0;
/// The text size on paper, in points (headings and the like scaled from it).
const TEXT_PT: f64 = 10.5;

/// What to print.
pub enum Content<'a> {
    /// A Markdown note, rendered: `folder` resolves relative pictures, `find` the files links
    /// and embeds name.
    Markdown { text: &'a str, folder: Option<gio::File>, find: &'a dyn Fn(&str) -> Option<gio::File> },
    /// Any other text, coloured as its `language`.
    Plain { text: &'a str, language: Option<sourceview::Language> },
}

/// Prints `content` (the printing dialog first), or with `pdf` writes it to that file as a PDF.
/// `layer` holds the view laid out for it meanwhile, out of sight.
pub async fn print(window: &gtk::ApplicationWindow, layer: &gtk::Overlay, title: &str, content: Content<'_>, pdf: Option<gio::File>) -> Result<(), String> {
    let setup = page_setup();
    let width = (setup.page_width(gtk::Unit::Points) * PX_PER_PT).round() as i32;
    let path = match &pdf {
        Some(file) => Some(file.path().ok_or("Only a file on this computer can be written to.")?),
        None => None,
    };
    let (view, placing) = lay_out(layer, &content, width);
    settle(&view, width).await;
    let picture = draw(layer, &view);
    let total = view.height();
    let tops = row_tops(&view);
    layer.remove_overlay(&view);
    layer.disconnect(placing);
    let Some(picture) = picture else { return Err("Nothing to print.".to_owned()) };

    let operation = gtk::PrintOperation::builder().job_name(title).default_page_setup(&setup).embed_page_setup(true).build();
    let starts = Rc::new(RefCell::new(Vec::new()));
    let starts_in = starts.clone();
    operation.connect_begin_print(move |operation, context| {
        // Fitted to the width of the paper chosen.
        let page = (context.height() * f64::from(width) / context.width()).floor() as i32;
        let pages = page_starts(&tops, total, page);
        operation.set_n_pages(pages.len() as i32);
        *starts_in.borrow_mut() = pages;
    });
    operation.connect_draw_page(move |_, context, page| {
        let starts = starts.borrow();
        let Some(&start) = starts.get(page as usize) else { return };
        let end = starts.get(page as usize + 1).copied().unwrap_or(total);
        let cairo = context.cairo_context();
        let scale = context.width() / f64::from(width);
        cairo.scale(scale, scale);
        // Only what is on this page, but as wide as anything is drawn (a code block's box
        // reaches into the margins).
        cairo.rectangle(-f64::from(width), 0.0, f64::from(3 * width), f64::from(end - start));
        cairo.clip();
        cairo.translate(0.0, -f64::from(start));
        picture.draw(&cairo);
    });
    let action = match path {
        Some(path) => {
            operation.set_export_filename(path);
            gtk::PrintOperationAction::Export
        }
        None => gtk::PrintOperationAction::PrintDialog,
    };
    operation.run(action, Some(window)).map(|_| ()).map_err(|error| error.to_string())
}

/// A4 or Letter (as the locale has it), upright, with even margins.
fn page_setup() -> gtk::PageSetup {
    let setup = gtk::PageSetup::new();
    setup.set_paper_size(&gtk::PaperSize::new(None));
    for set in [gtk::PageSetup::set_top_margin, gtk::PageSetup::set_bottom_margin, gtk::PageSetup::set_left_margin, gtk::PageSetup::set_right_margin] {
        set(&setup, MARGIN_MM, gtk::Unit::Mm);
    }
    setup
}

/// A view `width` pixels wide showing `content` in colours for paper, put in `layer` out of
/// sight, at its full height; and the handler placing it there.
fn lay_out(layer: &gtk::Overlay, content: &Content<'_>, width: i32) -> (Paper, glib::SignalHandlerId) {
    let view = Paper::new();
    view.add_css_class("nevis-print");
    // The rendered view's fonts, or the source view's: what pictures, tables and diagrams are
    // measured and drawn in.
    view.add_css_class(if matches!(content, Content::Markdown { .. }) { "nevis-read" } else { "nevis-editor" });
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_can_target(false);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_pixels_inside_wrap(3);
    style_paper();
    // Out of sight, as tall as its text, should the window lay it out too (see `settle`).
    let weak = view.downgrade();
    let placing = layer.connect_get_child_position(move |_, child| {
        let view = weak.upgrade().filter(|view| view.upcast_ref::<gtk::Widget>() == child)?;
        Some(place(width, view.height()))
    });
    layer.add_overlay(&view);
    let paper = theme::paper_colors();
    let scheme = sourceview::StyleSchemeManager::default().scheme("Adwaita");
    match content {
        Content::Markdown { text, folder, find } => {
            let measure = view.clone();
            let layout = move |text: &str| measure.create_pango_layout(Some(text));
            let rendered = theme::with_colors(paper.clone(), || render::render(text, folder.as_ref(), find, scheme.as_ref(), &layout, 0));
            view.set_buffer(Some(&rendered.buffer));
            view.set_panels(rendered.panels);
            view.set_marks(rendered.marks);
            for (anchor, widget) in &rendered.children {
                view.add_child_at_anchor(widget, anchor);
            }
            theme::with_colors(paper, || render::highlight_code(&rendered.buffer, &rendered.code, scheme.as_ref()));
        }
        Content::Plain { text, language } => {
            let buffer = sourceview::Buffer::new(None);
            buffer.set_language(language.as_ref());
            buffer.set_style_scheme(scheme.as_ref());
            buffer.set_text(text);
            buffer.ensure_highlight(&buffer.start_iter(), &buffer.end_iter());
            view.set_buffer(Some(&buffer));
        }
    }
    // The text's font, size and ink under every other tag: GTK gives a view's text its
    // stylesheet's only once the window draws, which a window out of sight doesn't.
    let buffer = view.buffer();
    let family = if matches!(content, Content::Markdown { .. }) { theme::fonts::BODY } else { theme::fonts::MONO };
    let ink = gtk::TextTag::builder()
        .family(family)
        .size_points(TEXT_PT)
        .foreground(theme::paper_colors().text.to_hex())
        .build();
    buffer.tag_table().add(&ink);
    ink.set_priority(0);
    buffer.apply_tag(&ink, &buffer.start_iter(), &buffer.end_iter());
    (view, placing)
}

/// The view on no background, at the text size for paper, its tables in paper colours: above
/// the palette's stylesheet.
fn style_paper() {
    thread_local! {
        static ADDED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    let Some(display) = gdk::Display::default() else { return };
    if ADDED.replace(true) {
        return;
    }
    let colors = theme::paper_colors();
    let (text, border, dim) = (colors.text.to_hex(), colors.border.to_hex(), colors.dim.to_hex());
    let css = gtk::CssProvider::new();
    css.load_from_string(&format!(
        "textview.nevis-print, textview.nevis-print > text {{ background: none; font-size: {TEXT_PT}pt; }}
         textview.nevis-print label {{ color: {text}; }}
         textview.nevis-print .nevis-table-head {{ color: {dim}; border-bottom-color: {border}; }}
         textview.nevis-print .nevis-table-cell {{ border-bottom-color: alpha({border}, 0.5); }}"
    ));
    gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_USER + 3);
}

/// Lays all of `view`'s text out, `width` pixels wide: GTK measures lines a screenful at a
/// time, so its height grows with each allocation until it stops. Allocated here, not by the
/// window, which lays out only when it is drawn.
async fn settle(view: &Paper, width: i32) {
    // One frame first, should the window draw (it styles the view), but not for long.
    let drawn = Rc::new(std::cell::Cell::new(false));
    let flag = drawn.clone();
    let tick = view.add_tick_callback(move |_, _| {
        flag.set(true);
        glib::ControlFlow::Break
    });
    for _ in 0..15 {
        if drawn.get() {
            break;
        }
        glib::timeout_future(Duration::from_millis(10)).await;
    }
    if !drawn.get() {
        tick.remove();
    }
    // Taller than any note first: allocated with its lines changed, GTK measures the lines on
    // screen at once, all of them then, rather than a few in the background between frames.
    // (A margin moved there and back is a change; unchanged, an allocation measures nothing.)
    let mut last = -1;
    for _ in 0..20 {
        let margin = view.left_margin();
        view.set_left_margin(margin + 1);
        view.set_left_margin(margin);
        view.size_allocate(&place(width, TALLEST), -1);
        let (_, height, _, _) = view.measure(gtk::Orientation::Vertical, width);
        view.size_allocate(&place(width, height), -1);
        if height == last {
            return;
        }
        last = height;
        glib::timeout_future(Duration::from_millis(10)).await;
    }
}

/// Taller than any note is laid out, in pixels: some 4,000 pages.
const TALLEST: i32 = 4_000_000;

/// Where the view is laid out: left of the window, out of sight.
fn place(width: i32, height: i32) -> gdk::Rectangle {
    gdk::Rectangle::new(-width - 1000, 0, width, height)
}

/// The picture of all of `view`, in colours for paper.
fn draw(layer: &gtk::Overlay, view: &Paper) -> Option<gtk::gsk::RenderNode> {
    // What was drawn before, in the palette's colours, is drawn again.
    fn redraw(widget: &gtk::Widget) {
        widget.queue_draw();
        let mut child = widget.first_child();
        while let Some(widget) = child {
            redraw(&widget);
            child = widget.next_sibling();
        }
    }
    redraw(view.upcast_ref());
    theme::with_colors(theme::paper_colors(), || {
        let snapshot = gtk::Snapshot::new();
        // Drawn as `layer` draws it, moved back from out of sight.
        snapshot.translate(&gtk::graphene::Point::new((view.width() + 1000) as f32, 0.0));
        layer.snapshot_child(view, &snapshot);
        snapshot.to_node()
    })
}

/// Where each row of text starts, down `view`: where a page may end.
fn row_tops(view: &Paper) -> Vec<i32> {
    let mut iter = view.buffer().start_iter();
    let mut tops = Vec::new();
    loop {
        // A paragraph's first row from the room above it; a wrapped row from its text.
        let y = if iter.starts_line() { view.line_yrange(&iter).0 } else { view.iter_location(&iter).y() };
        tops.push(view.buffer_to_window_coords(gtk::TextWindowType::Widget, 0, y).1);
        if !view.forward_display_line(&mut iter) {
            return tops;
        }
    }
}

/// Where each page starts, down a picture `total` pixels tall, pages `page` tall: at the
/// lowest row start (`tops`) that fits, or where the page is full when a row is taller.
fn page_starts(tops: &[i32], total: i32, page: i32) -> Vec<i32> {
    let mut starts = vec![0];
    let page = page.max(1);
    loop {
        let start = *starts.last().expect("a first page");
        let full = start + page;
        if full >= total {
            return starts;
        }
        starts.push(tops.iter().copied().filter(|&top| top > start && top <= full).max().unwrap_or(full));
    }
}

#[cfg(test)]
mod tests {
    use super::page_starts;

    #[test]
    fn pages_end_where_the_last_row_that_fits_starts() {
        // Rows of 30 on pages of 100: 0–90 fit the first page; the second starts at 90.
        let tops: Vec<i32> = (0..10).map(|row| row * 30).collect();
        assert_eq!(page_starts(&tops, 300, 100), vec![0, 90, 180, 270]);
    }

    #[test]
    fn a_row_taller_than_a_page_is_cut_where_the_page_is_full() {
        assert_eq!(page_starts(&[0, 250], 300, 100), vec![0, 100, 200]);
    }

    #[test]
    fn what_fits_on_one_page_is_one_page() {
        assert_eq!(page_starts(&[0, 30], 60, 100), vec![0]);
    }
}
