//! The text views' paper: what nevis draws behind and over the text. Behind it, each block's
//! panel: a callout's box, a bar down a quote's or an embedded note's side. A panel spans its
//! whole block whatever the margins of its lines; a paragraph background starts at each line's
//! own margin, so a list in a callout would leave notches in it. Over the text, in Live
//! Preview: dots, checkboxes, rules and callout titles where the markup under them is; and over
//! whole blocks (tables, pictures, flowcharts, formulas), the block drawn, its lines covered
//! (they stay, so each keeps its line number and the cursor goes into them as into any).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gtk::{gdk, gio, glib, graphene, gsk, pango, prelude::*, subclass::prelude::*};
use pulldown_cmark::Alignment;
use sourceview::subclass::prelude::*;

use crate::render::{self, Look, Panel};
use crate::style::{Block, Mark};
use crate::theme::{self, fonts};
use crate::{classes, devhooks, flowchart, gantt, sequence, state};

/// Finds a file a link names (as the rendered view does).
type Finder = Box<dyn Fn(&str) -> Option<gio::File>>;

/// Drawings by a block's kind, source and the text size (none where it can't be drawn).
type Drawings = HashMap<(Block, String, i32), Option<Rc<Drawing>>>;

/// A row of pictures under a line: the line, the pictures, and the room kept for them.
type Row = (Range<usize>, Vec<Rc<Drawing>>, i32);

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Paper {
        pub panels: RefCell<Vec<Panel>>,
        pub marks: RefCell<Vec<(Range<usize>, Mark)>>,
        /// Rounded backgrounds behind inline code and #tags: character ranges, sorted.
        pub chips: RefCell<Vec<(Range<usize>, gdk::RGBA)>>,
        /// Live Preview's blocks: those asked for (lines, kind, source), those drawn, and the
        /// room kept above the first line of those taller drawn than written.
        pub wanted: RefCell<Vec<(Range<usize>, Block, String)>>,
        pub blocks: RefCell<Vec<(Range<usize>, Rc<Drawing>)>>,
        /// Pictures beside other things: those asked for (their line, their sources), and those
        /// drawn in a row under it, with the room kept for them.
        pub wanted_rows: RefCell<Vec<(Range<usize>, Vec<String>)>>,
        pub rows: RefCell<Vec<Row>>,
        /// The room tags keep now: where, how much, and whether under the line.
        pub applied: RefCell<Vec<(Range<usize>, i32, bool)>>,
        pub drawings: RefCell<Drawings>,
        /// Tags keeping room above a line or under it, by which and how much.
        pub rooms: RefCell<HashMap<(bool, i32), gtk::TextTag>>,
        pub finder: RefCell<Option<Finder>>,
        /// Callout titles as Pango markup, by their Markdown.
        pub titles: RefCell<HashMap<String, String>>,
        /// Formulas laid out, by their LaTeX and the text size; the tag hiding what their
        /// drawing doesn't need of their source, and where it is now.
        pub formulas: RefCell<HashMap<(String, i32), pango::Layout>>,
        pub squeeze: RefCell<Option<gtk::TextTag>>,
        pub squeezed: RefCell<Vec<Range<usize>>>,
        /// The width and text size blocks were last placed for.
        pub width: Cell<i32>,
        pub size: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Paper {
        const NAME: &'static str = "NevisPaper";
        type Type = super::Paper;
        type ParentType = sourceview::View;
    }

    impl ObjectImpl for Paper {}
    impl ViewImpl for Paper {}

    impl WidgetImpl for Paper {
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            // Drawn blocks fit the new width (pictures shrink): their room again, once allocated.
            if self.width.replace(width) != width && self.obj().has_blocks() {
                let paper = self.obj().downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(paper) = paper.upgrade() {
                        paper.place_blocks();
                    }
                });
            }
        }
    }

    impl TextViewImpl for Paper {
        fn snapshot_layer(&self, layer: gtk::TextViewLayer, snapshot: gtk::Snapshot) {
            if layer == gtk::TextViewLayer::BelowText {
                super::draw_panels(&self.obj(), &self.panels.borrow(), &snapshot);
                super::draw_chips(&self.obj(), &self.chips.borrow(), &snapshot);
            }
            self.parent_snapshot_layer(layer, snapshot.clone());
            if layer == gtk::TextViewLayer::AboveText {
                super::draw_blocks(&self.obj(), &self.blocks.borrow(), &self.rows.borrow(), &snapshot);
                super::draw_marks(&self.obj(), &self.marks.borrow(), &snapshot);
                // The text size changed: blocks drawn again at the new size, once this is drawn.
                let size = self.obj().pango_context().font_description().map_or(0, |font| font.size());
                let formulas = self.marks.borrow().iter().any(|(_, mark)| matches!(mark, Mark::Math(_)));
                if size != self.size.get() && (self.obj().has_blocks() || formulas) {
                    self.size.set(size);
                    let paper = self.obj().downgrade();
                    glib::idle_add_local_once(move || {
                        if let Some(paper) = paper.upgrade() {
                            paper.place_blocks();
                            paper.squeeze_formulas();
                        }
                    });
                }
            }
        }
    }
}

glib::wrapper! {
    pub struct Paper(ObjectSubclass<imp::Paper>)
        @extends sourceview::View, gtk::TextView, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

impl Paper {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Shows `buffer` instead of the one shown now, emptied first. A view changing buffers
    /// keeps the lines GTK laid out but never measured (`place` asks about lines off screen);
    /// once the old buffer is freed they point at nothing, and the next frame or change
    /// crashes. Deleted text GTK forgets.
    pub fn show_buffer(&self, buffer: &impl IsA<gtk::TextBuffer>) {
        let old = self.buffer();
        if &old != buffer.upcast_ref::<gtk::TextBuffer>() {
            old.delete(&mut old.start_iter(), &mut old.end_iter());
            self.set_buffer(Some(buffer));
        }
    }

    /// The text at `x`, `y` (buffer coordinates), as `iter_at_location` without its crash: in
    /// the space under a line holding hidden text (Live Preview's markup, a fold), GTK counts
    /// the hidden bytes as shown and aborts, the index off the end of the line. So `y` is
    /// kept above that space, on the line's last row of text.
    pub fn iter_at(&self, x: i32, y: i32) -> Option<gtk::TextIter> {
        let (mut end, _) = self.line_at_y(y);
        if !end.ends_line() {
            end.forward_to_line_end();
        }
        let last = self.iter_location(&end);
        self.iter_at_location(x, y.min(last.y() + (last.height() - 1).max(0)))
    }

    /// Draws `panels`, marked in the buffer shown now, behind the text (the marks of those
    /// drawn before go).
    pub fn set_panels(&self, panels: Vec<Panel>) {
        let old = self.imp().panels.replace(panels);
        for panel in old {
            for mark in [panel.start, panel.end].into_iter().chain(panel.fold.map(|(_, body)| body)) {
                if let Some(buffer) = mark.buffer() {
                    buffer.delete_mark(&mark);
                }
            }
        }
        self.queue_draw();
    }

    /// Draws rounded backgrounds behind `chips` (sorted character ranges of the buffer shown
    /// now): a text tag's background has no padding and no corners.
    pub fn set_chips(&self, chips: Vec<(Range<usize>, gdk::RGBA)>) {
        if *self.imp().chips.borrow() != chips {
            *self.imp().chips.borrow_mut() = chips;
            self.queue_draw();
        }
    }

    /// Draws `marks` (sorted character ranges of the buffer shown now) over the text.
    pub fn set_marks(&self, marks: Vec<(Range<usize>, Mark)>) {
        let changed = {
            let mut have = self.imp().marks.borrow_mut();
            let changed = *have != marks;
            *have = marks;
            changed
        };
        if changed {
            self.squeeze_formulas();
            self.queue_draw();
        }
    }

    /// How much bigger than the body's a formula at `start` is drawn: as its line's characters
    /// are (a heading's are bigger).
    fn formula_scale(&self, start: &gtk::TextIter) -> f32 {
        let body = self.create_pango_layout(Some("X")).pixel_size().1.max(1) as f32;
        (place(self, start).height() as f32 / body).max(1.0)
    }

    /// A formula laid out as the rendered view writes it, at this view's text size (once).
    fn formula(&self, latex: &str) -> pango::Layout {
        let size = self.pango_context().font_description().map_or(0, |font| font.size());
        let key = (latex.to_owned(), size);
        if let Some(layout) = self.imp().formulas.borrow().get(&key) {
            return layout.clone();
        }
        let layout = self.create_pango_layout(None);
        layout.set_markup(&render::math_markup(latex, true));
        self.imp().formulas.borrow_mut().insert(key, layout.clone());
        layout
    }

    /// Of each formula drawn, keeps only as many characters of its source as the drawing is
    /// wide (the rest hidden), so its line closes up round it. Measured on the line itself, so
    /// a heading's bigger characters count as such.
    fn squeeze_formulas(&self) {
        let buffer = self.buffer();
        let mut want = Vec::new();
        for (range, mark) in self.imp().marks.borrow().iter() {
            let Mark::Math(latex) = mark else { continue };
            let start = buffer.iter_at_offset(range.start as i32);
            let mut second = start;
            second.forward_char();
            let (_, character) = span(self, &start, &second);
            if character < 1.0 {
                continue;
            }
            let wide = self.formula(latex).pixel_size().0 as f32 * self.formula_scale(&start) + 2.0;
            let keep = ((wide / character).ceil() as usize).max(1);
            if keep < range.len() {
                want.push(range.start + keep..range.end);
            }
        }
        if *self.imp().squeezed.borrow() == want {
            return;
        }
        let tag = self
            .imp()
            .squeeze
            .borrow_mut()
            .get_or_insert_with(|| {
                let tag = gtk::TextTag::builder().invisible(true).build();
                buffer.tag_table().add(&tag);
                tag
            })
            .clone();
        let (start, end) = buffer.bounds();
        buffer.remove_tag(&tag, &start, &end);
        for range in &want {
            buffer.apply_tag(&tag, &buffer.iter_at_offset(range.start as i32), &buffer.iter_at_offset(range.end as i32));
        }
        *self.imp().squeezed.borrow_mut() = want;
    }

    /// Finds the files pictures name with `finder`.
    pub fn find_with(&self, finder: impl Fn(&str) -> Option<gio::File> + 'static) {
        *self.imp().finder.borrow_mut() = Some(Box::new(finder));
    }

    /// Draws `blocks` (whole lines, sorted characters of the buffer shown now; see
    /// `style::blocks`) over their lines, and the pictures in `rows` (each line with its
    /// pictures' sources; see `style::pictures`) under their line: those it can draw.
    pub fn set_blocks(&self, blocks: Vec<(Range<usize>, Block, String)>, rows: Vec<(Range<usize>, Vec<String>)>) {
        *self.imp().wanted.borrow_mut() = blocks;
        *self.imp().wanted_rows.borrow_mut() = rows;
        self.place_blocks();
    }

    fn has_blocks(&self) -> bool {
        !self.imp().wanted.borrow().is_empty() || !self.imp().wanted_rows.borrow().is_empty()
    }

    /// Draws everything afresh (new colours).
    pub fn forget_drawings(&self) {
        for drawing in self.imp().drawings.borrow_mut().drain().filter_map(|(_, drawing)| drawing) {
            if let Drawing::Embed(_, _, view) = drawing.as_ref() {
                self.remove(view);
            }
        }
        self.imp().applied.borrow_mut().clear();
        self.place_blocks();
    }

    /// Keeps room above the first line of each block that is drawn taller than it is written.
    fn place_blocks(&self) {
        let imp = self.imp();
        imp.size.set(self.pango_context().font_description().map_or(0, |font| font.size()));
        let buffer = self.buffer();
        let room = (self.width() - self.left_margin() - self.right_margin()).max(64) as f32;
        // Embedded notes no longer asked for, or made at another text size, go, views and all.
        let size = imp.size.get();
        let embeds: Vec<String> = imp.wanted.borrow().iter().filter(|(_, block, _)| *block == Block::Embed).map(|(_, _, source)| source.clone()).collect();
        imp.drawings.borrow_mut().retain(|(block, source, at), drawing| {
            let keep = *block != Block::Embed || (*at == size && embeds.contains(source));
            if !keep && let Some(Drawing::Embed(_, _, view)) = drawing.as_deref() {
                self.remove(view);
            }
            keep
        });
        let mut drawn = Vec::new();
        let mut applied = Vec::new();
        for (range, block, source) in imp.wanted.borrow().iter() {
            let Some(drawing) = self.drawing(*block, source) else { continue };
            if let Drawing::Embed(_, _, view) = drawing.as_ref() {
                let width = (room - EMBED_INSET) as i32;
                if view.width_request() != width {
                    view.set_size_request(width, -1);
                }
            }
            let (first, last) = (buffer.iter_at_offset(range.start as i32), buffer.iter_at_offset(range.end as i32));
            let (top, bottom) = (place(self, &first), place(self, &last));
            let written = bottom.y() + bottom.height() - top.y();
            let keep = (drawing.size(room).1.ceil() as i32 + ABOVE + BELOW - written).max(0);
            applied.push((range.clone(), keep, false));
            drawn.push((range.clone(), drawing));
        }
        let mut rows = Vec::new();
        for (line, sources) in imp.wanted_rows.borrow().iter() {
            let pictures: Vec<Rc<Drawing>> = sources.iter().filter_map(|source| self.drawing(Block::Image, source)).collect();
            if pictures.is_empty() {
                continue;
            }
            let tall = row(&pictures, room).iter().map(|(_, tall)| *tall).fold(0.0, f32::max);
            let keep = tall.ceil() as i32 + ABOVE + BELOW;
            applied.push((line.clone(), keep, true));
            rows.push((line.clone(), pictures, keep));
        }
        if *imp.applied.borrow() != applied {
            let (start, end) = buffer.bounds();
            for tag in imp.rooms.borrow().values() {
                buffer.remove_tag(tag, &start, &end);
            }
            // Room under a line is kept above the next one instead where it can be: GTK aborts on
            // a click or a drag in the space under a line holding hidden text (see `iter_at`).
            let tops: Vec<usize> = applied.iter().filter(|(_, _, under)| !under).map(|(range, _, _)| range.start).collect();
            for &(ref range, keep, under) in applied.iter().filter(|(_, keep, _)| *keep > 0) {
                let mut first = buffer.iter_at_offset(range.start as i32);
                let mut next = first;
                next.forward_line();
                let mut under = under;
                if under && !next.is_end() && !tops.contains(&(next.offset() as usize)) {
                    first = next;
                    next.forward_line();
                    under = false;
                }
                let tag = imp
                    .rooms
                    .borrow_mut()
                    .entry((under, keep))
                    .or_insert_with(|| {
                        let tag = if under {
                            gtk::TextTag::builder().pixels_below_lines(keep).build()
                        } else {
                            gtk::TextTag::builder().pixels_above_lines(keep).build()
                        };
                        buffer.tag_table().add(&tag);
                        tag
                    })
                    .clone();
                buffer.apply_tag(&tag, &first, &next);
            }
            *imp.applied.borrow_mut() = applied;
        }
        *imp.blocks.borrow_mut() = drawn;
        *imp.rows.borrow_mut() = rows;
        self.queue_draw();
    }

    /// What `source` (a `block`) is drawn as, made once.
    fn drawing(&self, block: Block, source: &str) -> Option<Rc<Drawing>> {
        let size = self.pango_context().font_description().map_or(0, |font| font.size());
        let key = (block, source.to_owned(), size);
        if let Some(found) = self.imp().drawings.borrow().get(&key) {
            return found.clone();
        }
        let started = std::time::Instant::now();
        let made = self.make(block, source).map(Rc::new);
        if devhooks::active() {
            devhooks::lap(&format!("{block:?} drawn in {:.1} ms", started.elapsed().as_secs_f64() * 1000.0));
        }
        self.imp().drawings.borrow_mut().insert(key, made.clone());
        made
    }

    fn make(&self, block: Block, source: &str) -> Option<Drawing> {
        let finder = self.imp().finder.borrow();
        let find = |path: &str| finder.as_ref().and_then(|find| find(path));
        let text = |text: &str| self.body_layout(text);
        match block {
            Block::Image => {
                let (texture, width, height) = render::picture(source, &find)?;
                Some(Drawing::Picture(texture.upcast(), width as f32, height as f32, false))
            }
            Block::Chart => {
                let fence = |line: &&str| line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~");
                let code: Vec<&str> = source.lines().skip(1).take_while(|line| !fence(line)).collect();
                let code = code.join("\n");
                let picture = match flowchart::parse(&code).or_else(|| state::parse(&code)) {
                    Some(chart) => flowchart::picture(&chart, &text),
                    None => match (classes::parse(&code), gantt::parse(&code)) {
                        (Some(diagram), _) => classes::picture(&diagram, &text),
                        (_, Some(chart)) => gantt::picture(&chart, &text),
                        _ => sequence::picture(&sequence::parse(&code)?, &text),
                    },
                };
                let paintable = picture.paintable()?;
                let (width, height) = (paintable.intrinsic_width() as f32, paintable.intrinsic_height() as f32);
                Some(Drawing::Picture(paintable, width, height, true))
            }
            Block::Math => {
                let latex = source.trim().strip_prefix("$$")?.strip_suffix("$$")?.trim();
                let layout = text("");
                if let Some(mut font) = layout.font_description() {
                    font.set_size((f64::from(font.size()) * 1.15) as i32);
                    layout.set_font_description(Some(&font));
                }
                layout.set_markup(&render::math_markup(latex, false));
                layout.set_alignment(pango::Alignment::Center);
                Some(Drawing::Math(layout))
            }
            Block::Embed => {
                let (file, title, text) = render::embedded(source, &find)?;
                // The note as the rendered view shows it, in a view of its own beside this one's
                // text, out of sight, painted in place from there.
                let view = Paper::new();
                view.add_css_class("nevis-read");
                view.set_editable(false);
                view.set_cursor_visible(false);
                view.set_wrap_mode(gtk::WrapMode::WordChar);
                view.set_can_focus(false);
                view.set_can_target(false);
                self.add_overlay(&view, -100_000, 0);
                let layout = |text: &str| view.create_pango_layout(Some(text));
                let rendered = render::render(&text, file.parent().as_ref(), &find, theme::scheme().as_ref(), &layout, 0);
                view.set_buffer(Some(&rendered.buffer));
                view.set_panels(rendered.panels);
                for (anchor, widget) in &rendered.children {
                    view.add_child_at_anchor(widget, anchor);
                }
                render::highlight_code(&rendered.buffer, &rendered.code, theme::scheme().as_ref());
                let paintable = gtk::WidgetPaintable::new(Some(&view));
                // Its height known (or changed): room for it again, once laid out.
                let paper = self.downgrade();
                paintable.connect_invalidate_size(move |_| {
                    let paper = paper.clone();
                    glib::idle_add_local_once(move || {
                        if let Some(paper) = paper.upgrade() {
                            paper.place_blocks();
                        }
                    });
                });
                let paper = self.downgrade();
                paintable.connect_invalidate_contents(move |_| {
                    if let Some(paper) = paper.upgrade() {
                        paper.queue_draw();
                    }
                });
                let name = self.body_layout("");
                name.set_markup(&format!("<b>{}</b>", glib::markup_escape_text(&title)));
                Some(Drawing::Embed(name, paintable, view))
            }
            Block::Table => {
                let table = render::table(source, &find, &text)?;
                let digit = text("0").pixel_size().0.max(1);
                let (mut cells, mut columns, mut rows) = (Vec::new(), Vec::<f32>::new(), Vec::new());
                for (header, row) in &table.rows {
                    let mut laid = Vec::new();
                    let mut tallest = 0f32;
                    for (column, cell) in row.iter().enumerate() {
                        let layout = text("");
                        layout.set_markup(&if *header { format!("<b>{}</b>", cell.markup) } else { cell.markup.clone() });
                        // Long cells wrap, as in the rendered view.
                        if render::display_width(&cell.plain) > 30 {
                            layout.set_width(30 * digit * pango::SCALE);
                            layout.set_wrap(pango::WrapMode::WordChar);
                        }
                        let (width, height) = layout.pixel_size();
                        let rtl = pango::find_base_dir(&cell.plain) == pango::Direction::Rtl;
                        let xalign = match table.aligns.get(column) {
                            Some(Alignment::Center) => 0.5,
                            Some(Alignment::Right) => 1.0,
                            Some(Alignment::Left) => 0.0,
                            _ if rtl => 1.0,
                            _ => 0.0,
                        };
                        if columns.len() <= column {
                            columns.push(0.0);
                        }
                        columns[column] = columns[column].max(width as f32);
                        tallest = tallest.max(height as f32);
                        laid.push((layout, xalign));
                    }
                    rows.push(tallest);
                    cells.push(laid);
                }
                Some(Drawing::Table(cells, columns, rows))
            }
        }
    }

    /// A callout title's Markdown as Pango markup, bold, worked out once.
    fn title_markup(&self, title: &str) -> String {
        if let Some(markup) = self.imp().titles.borrow().get(title) {
            return markup.clone();
        }
        let finder = self.imp().finder.borrow();
        let find = |path: &str| finder.as_ref().and_then(|find| find(path));
        let markup = format!("<b>{}</b>", render::inline_markup(title, &find, &|text| self.body_layout(text)));
        self.imp().titles.borrow_mut().insert(title.to_owned(), markup.clone());
        markup
    }

    /// A layout for `text` in the rendered view's font, at this view's size.
    fn body_layout(&self, text: &str) -> pango::Layout {
        let layout = self.create_pango_layout(Some(text));
        let mut font = self.pango_context().font_description().unwrap_or_default();
        font.set_family(fonts::BODY);
        layout.set_font_description(Some(&font));
        layout
    }

    /// The checkbox drawn at `x`, `y` (in the view), as the range of its "[ ]". Told by where the
    /// box is drawn: next to hidden text, GTK names the wrong character at a box's edges.
    pub fn task_at(&self, x: f64, y: f64) -> Option<Range<usize>> {
        let (x, y) = self.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        let buffer = self.buffer();
        let marks = self.imp().marks.borrow();
        marks
            .iter()
            .filter(|(_, mark)| matches!(mark, Mark::Task(_)))
            .find(|(range, _)| {
                let start = buffer.iter_at_offset(range.start as i32);
                let line = place(self, &start);
                let (left, width) = span(self, &start, &buffer.iter_at_offset(range.end as i32));
                let (x, y) = (x as f32, y);
                x >= left - 2.0 && x <= left + width + 2.0 && y >= line.y() && y < line.y() + line.height()
            })
            .map(|(range, _)| range.clone())
    }
}

impl Default for Paper {
    fn default() -> Self {
        Self::new()
    }
}

/// How far a callout's box reaches past its text, above and below.
const PAD: i32 = 10;

/// Room above and below a drawn block; a table's padding, inside its border, and the space
/// between its columns and rows (as the rendered view's).
const ABOVE: i32 = 4;
const BELOW: i32 = 10;
const CELL_SIDE: f32 = 10.0;
const CELL_TOP: f32 = 6.0;
const COLUMN_GAP: f32 = 18.0;
const ROW_GAP: f32 = 6.0;
/// An embedded note's text, in from its bar, and below its name.
const EMBED_INSET: f32 = 16.0;
const NAME_GAP: f32 = 6.0;
/// Between pictures in a row under their line.
const PICTURE_GAP: f32 = 8.0;

/// The size of each of `pictures` in a row at most `room` wide: shrunk alike to fit.
fn row(pictures: &[Rc<Drawing>], room: f32) -> Vec<(f32, f32)> {
    let sizes: Vec<(f32, f32)> = pictures.iter().map(|picture| picture.size(room)).collect();
    let gaps = PICTURE_GAP * sizes.len().saturating_sub(1) as f32;
    let wide = sizes.iter().map(|(width, _)| width).sum::<f32>();
    let shrink = if wide + gaps > room { ((room - gaps) / wide).max(0.05) } else { 1.0 };
    sizes.into_iter().map(|(width, tall)| (width * shrink, tall * shrink)).collect()
}

/// What Live Preview draws for a block (see `style::blocks`).
pub enum Drawing {
    /// A picture or a flowchart, this big (smaller to fit), centred or not.
    Picture(gdk::Paintable, f32, f32, bool),
    /// A formula, centred.
    Math(pango::Layout),
    /// A table: its cells' text and where in its column each goes, the columns' widths, the
    /// rows' heights.
    Table(Vec<Vec<(pango::Layout, f32)>>, Vec<f32>, Vec<f32>),
    /// An embedded note: its name, and the note painted from the view showing it.
    Embed(pango::Layout, gtk::WidgetPaintable, Paper),
}

impl Drawing {
    /// How big it is drawn `room` pixels wide.
    fn size(&self, room: f32) -> (f32, f32) {
        let gaps = |n: usize, gap: f32| n.saturating_sub(1) as f32 * gap;
        match self {
            Drawing::Picture(_, width, height, _) if *width > room => (room, height * room / width),
            Drawing::Picture(_, width, height, _) => (*width, *height),
            Drawing::Math(layout) => {
                let (width, height) = layout.pixel_size();
                (width as f32, height as f32)
            }
            Drawing::Table(_, columns, rows) => (
                2.0 + 2.0 * CELL_SIDE + columns.iter().sum::<f32>() + gaps(columns.len(), COLUMN_GAP),
                2.0 + 2.0 * CELL_TOP + rows.iter().sum::<f32>() + gaps(rows.len(), ROW_GAP),
            ),
            Drawing::Embed(name, paintable, _) => (room, name.pixel_size().1 as f32 + NAME_GAP + paintable.intrinsic_height() as f32),
        }
    }
}

/// `blocks` over their lines (and the room above them), those on screen: the lines covered in
/// the page's colour, the drawing in the middle. And `rows` of pictures under their lines.
fn draw_blocks(view: &Paper, blocks: &[(Range<usize>, Rc<Drawing>)], rows: &[Row], snapshot: &gtk::Snapshot) {
    let buffer = view.buffer();
    let seen = view.visible_rect();
    let left = view.left_margin() as f32;
    let room = (seen.width() - view.left_margin() - view.right_margin()) as f32;
    let colors = theme::colors();
    for (line, pictures, keep) in rows {
        let (y, _) = view.line_yrange(&buffer.iter_at_offset(line.start as i32));
        // Under the line's text, whether its room is under it or above the next line.
        let end = place(view, &buffer.iter_at_offset(line.end as i32));
        let bottom = end.y() + end.height();
        if y > seen.y() + seen.height() || bottom + keep < seen.y() {
            continue;
        }
        let top = (bottom + ABOVE) as f32;
        let mut x = left;
        for (picture, (width, tall)) in pictures.iter().zip(row(pictures, room)) {
            if let Drawing::Picture(paintable, ..) = picture.as_ref() {
                snapshot.save();
                snapshot.translate(&graphene::Point::new(x.round(), top));
                paintable.snapshot(snapshot, f64::from(width), f64::from(tall));
                snapshot.restore();
            }
            x += width + PICTURE_GAP;
        }
    }
    for (range, drawing) in blocks {
        let (y, _) = view.line_yrange(&buffer.iter_at_offset(range.start as i32));
        let (last, last_height) = view.line_yrange(&buffer.iter_at_offset(range.end as i32));
        let height = last + last_height - y;
        if y > seen.y() + seen.height() || y + height < seen.y() {
            continue;
        }
        let cover = graphene::Rect::new(seen.x() as f32, y as f32, seen.width() as f32, height as f32);
        snapshot.append_color(&render::rgba(colors.background, 1.0), &cover);
        let (width, tall) = drawing.size(room);
        let top = (y as f32 + (height as f32 - tall) / 2.0).round();
        match drawing.as_ref() {
            Drawing::Picture(paintable, _, _, centred) => {
                let x = if *centred { left + (room - width) / 2.0 } else { left };
                snapshot.save();
                snapshot.translate(&graphene::Point::new(x.round(), top));
                paintable.snapshot(snapshot, f64::from(width), f64::from(tall));
                snapshot.restore();
            }
            Drawing::Math(layout) => {
                snapshot.save();
                snapshot.translate(&graphene::Point::new((left + (room - width) / 2.0).round(), top));
                snapshot.append_layout(layout, &render::rgba(colors.text, 1.0));
                snapshot.restore();
            }
            Drawing::Embed(name, paintable, _) => {
                // A bar down its side and its name, as the rendered view shows an embedded note.
                snapshot.append_color(&render::rgba(colors.link, 0.9), &graphene::Rect::new(left, top, 3.0, tall));
                snapshot.save();
                snapshot.translate(&graphene::Point::new(left + EMBED_INSET, top));
                snapshot.append_layout(name, &render::rgba(colors.link, 1.0));
                snapshot.translate(&graphene::Point::new(0.0, name.pixel_size().1 as f32 + NAME_GAP));
                paintable.snapshot(snapshot, f64::from(room - EMBED_INSET), f64::from(paintable.intrinsic_height()));
                snapshot.restore();
            }
            Drawing::Table(cells, columns, rows) => {
                let frame = graphene::Rect::new(left, top, width.min(room), tall);
                let rounded = gsk::RoundedRect::from_rect(frame, 6.0);
                snapshot.push_clip(&frame);
                snapshot.push_rounded_clip(&rounded);
                snapshot.append_color(&render::rgba(colors.surface, 1.0), &frame);
                snapshot.pop();
                let mut row_top = top + 1.0 + CELL_TOP;
                for (row, laid) in cells.iter().enumerate() {
                    let mut column_left = left + 1.0 + CELL_SIDE;
                    for ((layout, xalign), column) in laid.iter().zip(columns) {
                        let x = column_left + (column - layout.pixel_size().0 as f32) * xalign;
                        snapshot.save();
                        snapshot.translate(&graphene::Point::new(x.round(), row_top.round()));
                        snapshot.append_layout(layout, &render::rgba(colors.text, 1.0));
                        snapshot.restore();
                        column_left += column + COLUMN_GAP;
                    }
                    row_top += rows[row] + ROW_GAP;
                }
                let border = render::rgba(colors.border, 1.0);
                snapshot.append_border(&rounded, &[1.0; 4], &[border; 4]);
                snapshot.pop();
            }
        }
    }
}

/// Where the character at `iter` is drawn: its left edge (its right one in right-to-left
/// text), and its line's top and height. GTK's `iter_location` places a character as if the
/// hidden text before it on its line were shown; the cursor's place leaves it out, as the
/// drawing does.
fn place(view: &Paper, iter: &gtk::TextIter) -> gdk::Rectangle {
    view.cursor_locations(Some(iter)).0
}

/// From the left edge of the character at `start` to the right edge of the one before `end`,
/// whichever way the line runs: x and width.
fn span(view: &Paper, start: &gtk::TextIter, end: &gtk::TextIter) -> (f32, f32) {
    let (a, b) = (place(view, start).x() as f32, place(view, end).x() as f32);
    (a.min(b), (a - b).abs())
}

/// `panels` behind the text, in buffer coordinates: those on screen, outer ones first.
fn draw_panels(view: &Paper, panels: &[Panel], snapshot: &gtk::Snapshot) {
    let buffer = view.buffer();
    let seen = view.visible_rect();
    let left = view.left_margin() as f32;
    let right = (seen.x() + seen.width() - view.right_margin()) as f32;
    for panel in panels {
        let first = buffer.iter_at_mark(&panel.start);
        // A folded callout is its header.
        let mut last = match &panel.fold {
            Some((tag, body)) if tag.is_invisible() => buffer.iter_at_mark(body),
            _ => buffer.iter_at_mark(&panel.end),
        };
        if last.starts_line() && last.offset() > first.offset() {
            last.backward_char();
        }
        // Inside a folded callout: the line ending the block is hidden too.
        if last.offset() <= first.offset() || last.tags().iter().any(|tag| tag.is_invisible()) {
            continue;
        }
        let pad = match panel.look {
            Look::Box(_) => PAD,
            Look::Bar(_) => 1,
        };
        let (top, bottom) = (place(view, &first), place(view, &last));
        let y = top.y() - pad;
        let height = bottom.y() + bottom.height() + pad - y;
        if y > seen.y() + seen.height() || y + height < seen.y() {
            continue;
        }
        let (start, end) = (left + panel.inset as f32, right - panel.inset as f32);
        match &panel.look {
            Look::Box(color) => {
                let rect = graphene::Rect::new(start, y as f32, end - start, height as f32);
                snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(rect, 8.0));
                snapshot.append_color(color, &rect);
                snapshot.pop();
            }
            Look::Bar(color) => {
                let x = if panel.rtl { end - 3.0 } else { start };
                snapshot.append_color(color, &graphene::Rect::new(x, y as f32, 3.0, height as f32));
            }
        }
    }
}

/// `chips` behind the text, in buffer coordinates: those on screen, a piece on each line one
/// wraps over.
fn draw_chips(view: &Paper, chips: &[(Range<usize>, gdk::RGBA)], snapshot: &gtk::Snapshot) {
    if chips.is_empty() {
        return;
    }
    let buffer = view.buffer();
    let seen = view.visible_rect();
    let offset = |x: i32, y: i32| view.iter_at(x, y).map(|iter| iter.offset() as usize);
    let first = offset(seen.x(), seen.y()).unwrap_or(0);
    let last = offset(seen.x() + seen.width(), seen.y() + seen.height()).unwrap_or(usize::MAX);
    let from = chips.partition_point(|(range, _)| range.end < first);
    const PAD: f32 = 2.0;
    let piece = |start: &gtk::TextIter, end: &gtk::TextIter, color: &gdk::RGBA| {
        let at = place(view, start);
        let (left, width) = span(view, start, end);
        if width <= 0.0 {
            return;
        }
        let rect = graphene::Rect::new(left - PAD, at.y() as f32 + 1.0, width + 2.0 * PAD, (at.height() - 2).max(1) as f32);
        snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(rect, 4.0));
        snapshot.append_color(color, &rect);
        snapshot.pop();
    };
    for (range, color) in &chips[from..] {
        if range.start > last {
            break;
        }
        let (start, end) = (buffer.iter_at_offset(range.start as i32), buffer.iter_at_offset(range.end as i32));
        if place(view, &start).y() == place(view, &end).y() {
            piece(&start, &end, color);
            continue;
        }
        // Wrapped: a piece from each line's first character to its last.
        let (mut line_start, mut iter) = (start, start);
        let mut y = place(view, &start).y();
        while iter.offset() < end.offset() {
            let mut next = iter;
            next.forward_char();
            let next_y = if next.offset() < end.offset() { place(view, &next).y() } else { y };
            if next_y != y {
                piece(&line_start, &next, color);
                line_start = next;
                y = next_y;
            }
            iter = next;
        }
        piece(&line_start, &end, color);
    }
}

/// `marks` over the text, in buffer coordinates: those on screen.
fn draw_marks(view: &Paper, marks: &[(Range<usize>, Mark)], snapshot: &gtk::Snapshot) {
    if marks.is_empty() {
        return;
    }
    let buffer = view.buffer();
    let seen = view.visible_rect();
    let offset = |x: i32, y: i32| view.iter_at(x, y).map(|iter| iter.offset() as usize);
    let first = offset(seen.x(), seen.y()).unwrap_or(0);
    let last = offset(seen.x() + seen.width(), seen.y() + seen.height()).unwrap_or(usize::MAX);
    let from = marks.partition_point(|(range, _)| range.end < first);
    let colors = theme::colors();
    for (range, mark) in &marks[from..] {
        if range.start > last {
            break;
        }
        let start = buffer.iter_at_offset(range.start as i32);
        let at = place(view, &start);
        let (x, y, height) = (at.x() as f32, at.y() as f32, at.height() as f32);
        match mark {
            Mark::Bullet => {
                let mut next = start;
                next.forward_char();
                let (left, width) = span(view, &start, &next);
                let r = (height * 0.12).max(2.5);
                let (cx, cy) = (left + width / 2.0, y + height / 2.0);
                let dot = graphene::Rect::new(cx - r, cy - r, 2.0 * r, 2.0 * r);
                snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(dot, r));
                snapshot.append_color(&render::rgba(colors.bullet, 1.0), &dot);
                snapshot.pop();
            }
            Mark::Task(done) => {
                let (left, width) = span(view, &start, &buffer.iter_at_offset(range.end as i32));
                let side = (height * 0.7).min(width).round();
                let (left, top) = ((left + (width - side) / 2.0).round(), (y + (height - side) / 2.0).round());
                let square = graphene::Rect::new(left, top, side, side);
                let rounded = gsk::RoundedRect::from_rect(square, 3.0);
                if *done {
                    snapshot.push_rounded_clip(&rounded);
                    snapshot.append_color(&render::rgba(colors.accent, 1.0), &square);
                    snapshot.pop();
                    let tick = gsk::PathBuilder::new();
                    tick.move_to(left + side * 0.25, top + side * 0.52);
                    tick.line_to(left + side * 0.43, top + side * 0.70);
                    tick.line_to(left + side * 0.76, top + side * 0.32);
                    let stroke = gsk::Stroke::new(side * 0.13);
                    stroke.set_line_cap(gsk::LineCap::Round);
                    stroke.set_line_join(gsk::LineJoin::Round);
                    snapshot.append_stroke(&tick.to_path(), &stroke, &render::rgba(colors.background, 1.0));
                } else {
                    let color = render::rgba(colors.dim, 1.0);
                    snapshot.append_border(&rounded, &[1.5; 4], &[color; 4]);
                }
            }
            Mark::Rule => {
                let left = view.left_margin() as f32;
                let right = (seen.x() + seen.width() - view.right_margin()) as f32;
                let line = graphene::Rect::new(left, (y + height / 2.0).round(), right - left, 1.0);
                snapshot.append_color(&render::rgba(colors.rule, 1.0), &line);
            }
            Mark::Math(latex) => {
                // Over what is kept of its source: as wide as it is, or shrunk to fit.
                let layout = view.formula(latex);
                let (left, kept) = span(view, &start, &buffer.iter_at_offset(range.end as i32));
                let (wide, tall) = layout.pixel_size();
                let mut scale = view.formula_scale(&start);
                if wide as f32 * scale > kept && kept > 0.0 {
                    scale = kept / wide as f32;
                }
                snapshot.save();
                snapshot.translate(&graphene::Point::new(left, y + (height - tall as f32 * scale) / 2.0));
                snapshot.scale(scale, scale);
                snapshot.append_layout(&layout, &render::rgba(colors.text, 1.0));
                snapshot.restore();
            }
            Mark::Title(title, kind) => {
                let layout = view.create_pango_layout(None);
                layout.set_markup(&view.title_markup(title));
                snapshot.save();
                snapshot.translate(&graphene::Point::new(x, y));
                snapshot.append_layout(&layout, &render::rgba(render::callout_color(kind), 1.0));
                snapshot.restore();
            }
        }
    }
}
