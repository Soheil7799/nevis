//! Markdown styling for the source view: headings by level, dimmed markup characters,
//! bold, italic, links, code and quotes. The text stays exactly as typed; only its look changes.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::time::Instant;

use gtk::{gdk, glib, pango, prelude::*};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Tag, TagEnd};

use crate::devhooks;
use crate::paper::Paper;
use crate::theme;
use crate::{links, obsidian, render};

/// How a stretch of Markdown source is drawn. Where kinds overlap, the later one wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Quote,
    CodeBlock,
    Frontmatter,
    Html,
    H1,
    H2,
    H3,
    H4,
    H5,
    H6,
    Strong,
    Emphasis,
    Strike,
    Highlight,
    Code,
    LinkText,
    Tag,
    ListMarker,
    Task,
    Url,
    Comment,
    Marker,
}

pub const KINDS: usize = Kind::Marker as usize + 1;

/// Every kind, in `Kind` order (which is also tag priority: later wins).
const ALL: [Kind; KINDS] = [
    Kind::Quote,
    Kind::CodeBlock,
    Kind::Frontmatter,
    Kind::Html,
    Kind::H1,
    Kind::H2,
    Kind::H3,
    Kind::H4,
    Kind::H5,
    Kind::H6,
    Kind::Strong,
    Kind::Emphasis,
    Kind::Strike,
    Kind::Highlight,
    Kind::Code,
    Kind::LinkText,
    Kind::Tag,
    Kind::ListMarker,
    Kind::Task,
    Kind::Url,
    Kind::Comment,
    Kind::Marker,
];

/// Larger documents stay plain: they would be re-parsed on every keystroke.
const MAX_STYLED_BYTES: usize = 512 * 1024;

/// A code block's colours: character ranges and what colour each is.
type Colours = Rc<Vec<(Range<usize>, gdk::RGBA)>>;

/// Keeps a buffer's Markdown styled as it is edited.
pub struct Styler {
    buffer: sourceview::Buffer,
    /// One tag per kind, in `ALL` order.
    tags: Vec<gtk::TextTag>,
    enabled: Cell<bool>,
    queued: Cell<bool>,
    /// Fenced code coloured as its language: colours by language and code (so a block is
    /// worked out again only when it changes), a tag per colour, and what was coloured last.
    colours: RefCell<HashMap<(String, String), Colours>>,
    colour_tags: RefCell<HashMap<String, gtk::TextTag>>,
    coloured: RefCell<Vec<(String, String)>>,
    /// With spell checking on: the tag spell checking skips, kept on code and Persian.
    unspelled: RefCell<Option<gtk::TextTag>>,
    /// Live Preview: the tag that hides markup, whether it is on, the text and spans styled
    /// last (what is hidden is worked out from them), the lines being edited when it was, and
    /// whether it waits (typing through an input method, dragging a selection).
    hide: gtk::TextTag,
    fence: gtk::TextTag,
    /// Live Preview draws on this view (dots, checkboxes, rules and titles over the text,
    /// quote bars and callout boxes behind it); the tag keeps the text under a drawing unseen.
    paper: RefCell<Option<Paper>>,
    clear: gtk::TextTag,
    live: Cell<bool>,
    styled: RefCell<Option<(String, Spans)>>,
    editing: Cell<Option<(usize, usize)>>,
    composing: Cell<bool>,
    dragging: Cell<bool>,
}

impl Styler {
    pub fn new(buffer: &sourceview::Buffer) -> Rc<Self> {
        let table = buffer.tag_table();
        let tags = ALL
            .iter()
            .map(|&kind| {
                let tag = tag_for(kind);
                table.add(&tag);
                tag
            })
            .collect();
        let hide = gtk::TextTag::builder().invisible(true).build();
        table.add(&hide);
        // A hidden fence's line, nearly no height: its block shows as just the code.
        let fence = gtk::TextTag::builder().size_points(1.0).pixels_above_lines(0).build();
        table.add(&fence);
        // Not quite 0: Pango takes an alpha of 0 as none given, and draws the text (or its
        // background) opaque. No background (a formula's code background would show round its
        // drawing).
        let unseen = gdk::RGBA::new(0.0, 0.0, 0.0, 0.001);
        let clear = gtk::TextTag::builder().foreground_rgba(&unseen).background_rgba(&unseen).build();
        table.add(&clear);
        let styler = Rc::new(Self {
            buffer: buffer.clone(),
            hide,
            fence,
            paper: RefCell::new(None),
            clear,
            live: Cell::new(false),
            styled: RefCell::new(None),
            editing: Cell::new(None),
            composing: Cell::new(false),
            dragging: Cell::new(false),
            tags,
            enabled: Cell::new(false),
            queued: Cell::new(false),
            colours: RefCell::new(HashMap::new()),
            colour_tags: RefCell::new(HashMap::new()),
            coloured: RefCell::new(Vec::new()),
            unspelled: RefCell::new(None),
        });
        let weak = Rc::downgrade(&styler);
        buffer.connect_changed(move |_| {
            if let Some(styler) = weak.upgrade() {
                styler.queue();
            }
        });
        styler
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.get()
    }

    /// Whether Live Preview is on for this text (Markdown, styled).
    pub fn is_live(&self) -> bool {
        self.live.get() && self.enabled.get()
    }

    /// Whether `line` is being edited: the cursor's, or one the selection touches.
    pub fn is_editing(&self, line: usize) -> bool {
        self.editing_lines().is_some_and(|(first, last)| (first..=last).contains(&line))
    }

    /// The span of one of `kinds` at character `offset`, as styled last.
    pub fn span_at(&self, kinds: &[Kind], offset: usize) -> Option<Range<usize>> {
        let styled = self.styled.borrow();
        let (_, spans) = styled.as_ref()?;
        kinds.iter().find_map(|&kind| {
            let ranges = spans.of(kind);
            let after = ranges.partition_point(|range| range.end <= offset);
            ranges.get(after).filter(|range| range.start <= offset).cloned()
        })
    }

    /// Live Preview on or off: markup hidden except where the cursor is.
    pub fn set_live(&self, on: bool) {
        self.live.set(on);
        self.mark_live();
        self.editing.set(None);
        self.update_panels();
        self.rehide();
    }

    /// The line numbers were shown or hidden: fences shrink only without them.
    pub fn numbers_changed(&self) {
        self.rehide();
    }

    /// Live Preview draws on `paper`, the view showing the buffer.
    pub fn draw_on(&self, paper: &Paper) {
        *self.paper.borrow_mut() = Some(paper.clone());
        self.mark_live();
    }

    /// Live Preview sets prose in the body font, code in the code font (see `apply_font`).
    fn mark_live(&self) {
        if let Some(paper) = self.paper.borrow().as_ref() {
            if self.is_live() {
                paper.add_css_class("nevis-live");
            } else {
                paper.remove_css_class("nevis-live");
            }
        }
    }

    /// The cursor or the selection moved: the lines shown as written may change.
    pub fn cursor_moved(&self) {
        if self.live.get() && self.editing_lines() != self.editing.get() {
            self.rehide();
        }
    }

    /// While text is composed through an input method, or a selection dragged, what is hidden
    /// stays as it is (text moving under the pointer or the composition would be a mess).
    pub fn set_composing(&self, on: bool) {
        self.composing.set(on);
        if !on {
            self.rehide();
        }
    }

    pub fn set_dragging(&self, on: bool) {
        self.dragging.set(on);
        if !on {
            self.rehide();
        }
    }

    /// The first and last lines of the selection, or the cursor's line.
    fn editing_lines(&self) -> Option<(usize, usize)> {
        let (start, end) = self.buffer.selection_bounds().unwrap_or_else(|| {
            let cursor = self.buffer.iter_at_mark(&self.buffer.get_insert());
            (cursor, cursor)
        });
        Some((start.line() as usize, end.line() as usize))
    }

    /// Hides the markup Live Preview hides now (`hidden`), changing only what differs.
    fn rehide(&self) {
        if self.composing.get() || self.dragging.get() {
            return;
        }
        let editing = self.editing_lines();
        self.editing.set(editing);
        // Beside line numbers a fence keeps its height: its number would sit on the next one's.
        let numbered = self.paper.borrow().as_ref().is_some_and(sourceview::prelude::ViewExt::shows_line_numbers);
        let (want, fences, marks, blocks, rows) = match (&*self.styled.borrow(), editing) {
            (Some((text, spans)), Some((first, last))) if self.live.get() && self.enabled.get() => {
                let (blocks, rows) = (blocks(text, spans, first..=last), pictures(text, spans, first..=last));
                // Each block's and picture's source, for the paper to draw it from.
                let bytes: Vec<usize> = if blocks.is_empty() && rows.is_empty() {
                    Vec::new()
                } else {
                    text.char_indices().map(|(i, _)| i).chain([text.len()]).collect()
                };
                let source = |range: &Range<usize>| text[bytes[range.start]..bytes[range.end]].to_owned();
                let blocks = blocks
                    .into_iter()
                    .map(|(range, block)| {
                        let text = source(&range);
                        (range, block, text)
                    })
                    .collect();
                let rows = rows.into_iter().map(|(line, pictures)| (line, pictures.iter().map(source).collect())).collect();
                let fences = if numbered { Vec::new() } else { fences(text, spans, first..=last) };
                (hidden(text, spans, first..=last), fences, drawn(text, spans, first..=last), blocks, rows)
            }
            _ => (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        };
        self.retag(&self.hide, &want);
        self.retag(&self.fence, &fences);
        // Under a drawing the text stays, unseen (a title's line is hidden instead).
        let under: Vec<Range<usize>> = marks.iter().map(|(range, _)| range.clone()).filter(|range| !range.is_empty()).collect();
        self.retag(&self.clear, &under);
        if let Some(paper) = &*self.paper.borrow() {
            paper.set_marks(marks);
            paper.set_blocks(blocks, rows);
        }
    }

    /// Puts `tag` on `want` (sorted character ranges) and nowhere else, changing only what
    /// differs.
    fn retag(&self, tag: &gtk::TextTag, want: &[Range<usize>]) {
        let have = tagged(&self.buffer, tag);
        for range in subtract(&have, want) {
            self.buffer.remove_tag(tag, &self.iter_at(range.start), &self.iter_at(range.end));
        }
        for range in subtract(want, &have) {
            self.buffer.apply_tag(tag, &self.iter_at(range.start), &self.iter_at(range.end));
        }
    }

    /// The panels for what was styled last: a box behind each code block, and in Live Preview,
    /// a bar beside each quote and a box behind each callout. Out in the margins, the text
    /// staying where it is.
    fn update_panels(&self) {
        let Some(paper) = self.paper.borrow().clone() else { return };
        let colors = theme::colors();
        let (panels, code, chips) = match &*self.styled.borrow() {
            Some((text, spans)) if self.enabled.get() => {
                let quotes = if self.live.get() { panels(text, spans) } else { Vec::new() };
                (quotes, spans.of(Kind::CodeBlock).to_vec(), chips(spans, &colors))
            }
            _ => (Vec::new(), Vec::new(), Vec::new()),
        };
        paper.set_chips(chips);
        let code = code.into_iter().map(|range| render::Panel {
            start: self.buffer.create_mark(None, &self.iter_at(range.start), true),
            end: self.buffer.create_mark(None, &self.iter_at(range.end), false),
            inset: -12,
            look: render::Look::Box(render::rgba(colors.code_background, 1.0)),
            rtl: false,
            fold: None,
        });
        let accent = colors.accent;
        let panels = panels
            .into_iter()
            .map(|(range, kind)| {
                let (start, end) = (self.iter_at(range.start), self.iter_at(range.end));
                let rtl = pango::find_base_dir(&self.buffer.text(&start, &end, true)) == pango::Direction::Rtl;
                render::Panel {
                    start: self.buffer.create_mark(None, &start, true),
                    end: self.buffer.create_mark(None, &end, false),
                    inset: -12,
                    look: match kind {
                        Some(kind) => render::Look::Box(render::rgba(render::callout_color(kind), 0.10)),
                        None => render::Look::Bar(render::rgba(accent, 0.7)),
                    },
                    rtl,
                    fold: None,
                }
            })
            .chain(code)
            .collect();
        paper.set_panels(panels);
    }

    /// Whether `iter` is inside a code block or the front matter, where text is taken as is.
    pub fn in_code(&self, iter: &gtk::TextIter) -> bool {
        ALL.iter()
            .zip(&self.tags)
            .any(|(kind, tag)| matches!(kind, Kind::CodeBlock | Kind::Frontmatter) && iter.has_tag(tag))
    }

    /// Takes the colours of the palette now in use.
    pub fn recolor(&self) {
        for (&kind, tag) in ALL.iter().zip(&self.tags) {
            let fresh = tag_for(kind);
            tag.set_foreground_rgba(fresh.is_foreground_set().then(|| fresh.foreground_rgba()).flatten().as_ref());
            tag.set_background_rgba(fresh.is_background_set().then(|| fresh.background_rgba()).flatten().as_ref());
            tag.set_paragraph_background_rgba(
                fresh.is_paragraph_background_set().then(|| fresh.paragraph_background_rgba()).flatten().as_ref(),
            );
        }
        // Code takes the new scheme's colours, and so do the blocks Live Preview draws.
        self.uncolour();
        self.colours.borrow_mut().clear();
        if let Some(paper) = &*self.paper.borrow() {
            paper.forget_drawings();
        }
        if self.enabled.get() {
            self.restyle();
        }
    }

    /// Styles the buffer from now on (for Markdown files), or removes all styling.
    pub fn set_enabled(&self, on: bool) {
        self.enabled.set(on);
        self.mark_live();
        if on {
            self.restyle();
        } else {
            self.clear();
        }
    }

    /// Restyles once the current batch of edits is done, before the next redraw,
    /// so typed text never shows unstyled for a frame.
    fn queue(self: &Rc<Self>) {
        if !self.enabled.get() || self.queued.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_full(glib::Priority::HIGH_IDLE, move || {
            if let Some(styler) = weak.upgrade() {
                styler.queued.set(false);
                styler.restyle();
            }
            glib::ControlFlow::Break
        });
    }

    /// Brings the tags in line with a fresh parse, touching only what changed:
    /// re-tagging the whole text would make GTK lay all of it out again.
    fn restyle(&self) {
        let started = Instant::now();
        let text = self.buffer.text(&self.buffer.start_iter(), &self.buffer.end_iter(), true);
        if text.len() > MAX_STYLED_BYTES {
            return self.clear();
        }
        let wanted = spans(&text);
        for (kind, tag) in ALL.iter().zip(&self.tags) {
            self.retag(tag, wanted.of(*kind));
        }
        self.colour_code(wanted.fences());
        if let Some(tag) = self.unspelled.borrow().as_ref() {
            self.retag(tag, &unspelled(&text, &wanted));
        }
        *self.styled.borrow_mut() = Some((text.to_string(), wanted.clone()));
        self.update_panels();
        self.rehide();
        if devhooks::active() {
            devhooks::lap(&format!("markdown styled in {:.1} ms", started.elapsed().as_secs_f64() * 1000.0));
            for (kind, tag) in ALL.iter().zip(&self.tags) {
                if tagged(&self.buffer, tag) != wanted.of(*kind) {
                    eprintln!("nevis: {kind:?} tags out of step with the text");
                }
            }
        }
    }

    /// Keeps `tag` on what spell checking should skip: code, addresses, and Persian (or any
    /// Arabic-script) words, which nevis has no dictionary for.
    pub fn skip_spelling(&self, tag: gtk::TextTag) {
        *self.unspelled.borrow_mut() = Some(tag);
        if self.is_enabled() {
            self.restyle();
        }
    }

    fn clear(&self) {
        let (start, end) = (self.buffer.start_iter(), self.buffer.end_iter());
        for tag in &self.tags {
            self.buffer.remove_tag(tag, &start, &end);
        }
        self.styled.borrow_mut().take();
        self.buffer.remove_tag(&self.hide, &start, &end);
        self.buffer.remove_tag(&self.fence, &start, &end);
        self.buffer.remove_tag(&self.clear, &start, &end);
        self.update_panels();
        if let Some(paper) = &*self.paper.borrow() {
            paper.set_marks(Vec::new());
        }
        self.uncolour();
    }

    /// Colours each fenced code block as its language, when any code changed since last time
    /// (moving text around doesn't count: tags move with it).
    fn colour_code(&self, fences: &[Fence]) {
        let codes: Vec<(String, String)> = fences
            .iter()
            .map(|fence| {
                let code = fence
                    .pieces
                    .iter()
                    .map(|p| self.buffer.text(&self.iter_at(p.start), &self.iter_at(p.end), true).to_string())
                    .collect();
                (fence.language.clone(), code)
            })
            .collect();
        if *self.coloured.borrow() == codes {
            return;
        }
        self.uncolour();
        let scheme = theme::scheme();
        let mut cache = self.colours.borrow_mut();
        cache.retain(|key, _| codes.contains(key));
        for (fence, key) in fences.iter().zip(&codes) {
            let colours = cache
                .entry(key.clone())
                .or_insert_with(|| Rc::new(render::code_colours(&key.0, &key.1, scheme.as_ref())))
                .clone();
            for (range, colour) in colours.iter() {
                let tag = self.colour_tag(colour);
                // The code is the pieces one after another; back to where each piece is.
                let mut before = 0;
                for piece in &fence.pieces {
                    let len = piece.end - piece.start;
                    let (from, to) = (range.start.max(before), range.end.min(before + len));
                    if from < to {
                        let at = |offset: usize| self.iter_at(piece.start + offset - before);
                        self.buffer.apply_tag(&tag, &at(from), &at(to));
                    }
                    before += len;
                }
            }
        }
        *self.coloured.borrow_mut() = codes;
    }

    fn colour_tag(&self, colour: &gdk::RGBA) -> gtk::TextTag {
        self.colour_tags
            .borrow_mut()
            .entry(colour.to_string())
            .or_insert_with(|| {
                let tag = gtk::TextTag::builder().foreground_rgba(colour).build();
                self.buffer.tag_table().add(&tag);
                tag
            })
            .clone()
    }

    /// Takes the code colours off, everywhere.
    fn uncolour(&self) {
        let (start, end) = (self.buffer.start_iter(), self.buffer.end_iter());
        for tag in self.colour_tags.borrow().values() {
            self.buffer.remove_tag(tag, &start, &end);
        }
        self.coloured.borrow_mut().clear();
    }

    fn iter_at(&self, offset: usize) -> gtk::TextIter {
        self.buffer.iter_at_offset(offset as i32)
    }
}

/// How each kind looks: Ayu Mirage colours, sizes for headings.
fn tag_for(kind: Kind) -> gtk::TextTag {
    let heading = |scale: f64| gtk::TextTag::builder().weight(650).scale(scale).foreground(theme::colors().heading.to_hex()).build();
    let tint = |color: crate::color::Rgb, alpha: f32| gdk::RGBA::new(color.r as f32, color.g as f32, color.b as f32, alpha);
    match kind {
        Kind::Highlight => gtk::TextTag::builder().background_rgba(&tint(theme::colors().changed, 0.32)).build(),
        // Their backgrounds are chips the view draws (see `update_panels`).
        Kind::Tag => gtk::TextTag::builder().foreground(theme::colors().accent.to_hex()).build(),
        Kind::Comment => gtk::TextTag::builder().foreground(theme::colors().dim.to_hex()).style(pango::Style::Italic).build(),
        Kind::Quote => gtk::TextTag::builder().foreground(theme::colors().quote.to_hex()).build(),
        // Its background is a panel the view draws (see `update_panels`): a paragraph background
        // leaves out blank lines, splitting a block in two.
        Kind::CodeBlock => gtk::TextTag::builder()
            .family(theme::fonts::MONO)
            .foreground(theme::colors().code.to_hex())
            .direction(gtk::TextDirection::Ltr)
            .build(),
        Kind::Frontmatter | Kind::Html => gtk::TextTag::builder().family(theme::fonts::MONO).foreground(theme::colors().dim.to_hex()).build(),
        Kind::Url => gtk::TextTag::builder().foreground(theme::colors().dim.to_hex()).build(),
        Kind::H1 => heading(1.6),
        Kind::H2 => heading(1.3),
        Kind::H3 => heading(1.12),
        Kind::H4 | Kind::H5 => heading(1.0),
        Kind::H6 => heading(0.9),
        Kind::Strong => gtk::TextTag::builder().weight(700).build(),
        Kind::Emphasis => gtk::TextTag::builder().style(pango::Style::Italic).build(),
        Kind::Strike => gtk::TextTag::builder().strikethrough(true).foreground(theme::colors().dim.to_hex()).build(),
        Kind::Code => gtk::TextTag::builder().family(theme::fonts::MONO).foreground(theme::colors().code.to_hex()).build(),
        Kind::LinkText => gtk::TextTag::builder().foreground(theme::colors().link.to_hex()).underline(pango::Underline::Single).build(),
        Kind::ListMarker => gtk::TextTag::builder().foreground(theme::colors().bullet.to_hex()).weight(700).build(),
        Kind::Task => gtk::TextTag::builder().foreground(theme::colors().accent.to_hex()).weight(700).build(),
        Kind::Marker => gtk::TextTag::builder().foreground(theme::colors().marker.to_hex()).build(),
    }
}

/// What spell checking skips in `text`, styled as `spans`: code, links' addresses and
/// written-out ones, front matter, HTML and Arabic-script words; character ranges, sorted and
/// merged.
fn unspelled(text: &str, spans: &Spans) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = [Kind::Code, Kind::CodeBlock, Kind::Url, Kind::Frontmatter, Kind::Html, Kind::Tag]
        .iter()
        .flat_map(|&kind| spans.of(kind).iter().cloned())
        .collect();
    let arabic = |c: char| matches!(c, '\u{0600}'..='\u{06FF}' | '\u{0750}'..='\u{077F}' | '\u{08A0}'..='\u{08FF}'
        | '\u{FB50}'..='\u{FDFF}' | '\u{FE70}'..='\u{FEFF}' | '\u{200C}');
    let mut start = None;
    for (offset, c) in text.chars().enumerate() {
        match (arabic(c), start) {
            (true, None) => start = Some(offset),
            (false, Some(from)) => {
                ranges.push(from..offset);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        ranges.push(from..text.chars().count());
    }
    // Addresses written out (https://…, www.…): Markdown only marks <https://…> as links.
    let (mut word, mut chars) = (None, 0);
    for (byte, c) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
        if c.is_whitespace() {
            if let Some((from, from_byte)) = word.take() {
                let written = &text[from_byte..byte];
                if written.contains("://") || written.starts_with("www.") {
                    ranges.push(from..chars);
                }
            }
        } else if word.is_none() {
            word = Some((chars, byte));
        }
        chars += 1;
    }
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// The character ranges `tag` currently covers, in order.
fn tagged(buffer: &sourceview::Buffer, tag: &gtk::TextTag) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut iter = buffer.start_iter();
    let mut open = iter.has_tag(tag).then_some(0);
    while iter.forward_to_tag_toggle(Some(tag)) {
        let offset = iter.offset() as usize;
        match open.take() {
            Some(start) => ranges.push(start..offset),
            None => open = Some(offset),
        }
    }
    if let Some(start) = open {
        ranges.push(start..buffer.char_count() as usize);
    }
    ranges
}

/// The parts of `a` not covered by `b`; both sorted and non-overlapping.
fn subtract(a: &[Range<usize>], b: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut first = 0;
    for range in a {
        while first < b.len() && b[first].end <= range.start {
            first += 1;
        }
        let mut start = range.start;
        for cut in b[first..].iter().take_while(|cut| cut.start < range.end) {
            if cut.start > start {
                out.push(start..cut.start);
            }
            start = start.max(cut.end);
        }
        if start < range.end {
            out.push(start..range.end);
        }
    }
    out
}

/// Character ranges (not bytes) for each kind, sorted and merged; and the fenced code blocks
/// that name a language, to colour as that language.
#[derive(Debug, Default)]
#[derive(Clone)]
pub struct Spans {
    /// Character ranges by kind, in `Kind` order.
    kinds: [Vec<Range<usize>>; KINDS],
    fences: Vec<Fence>,
    /// Tables, math and rules: left as written by the hiding (see `as_written`).
    written: Vec<Range<usize>>,
    /// Blocks Live Preview may draw (see `blocks`), pictures beside other things (see
    /// `pictures`), and formulas in lines with other things (see `drawn`).
    blocks: Vec<(Range<usize>, Block)>,
    pictures: Vec<Range<usize>>,
    formulas: Vec<(Range<usize>, String)>,
}

/// A fenced code block's language, and the character ranges its code is in: one, or a piece
/// per line inside a quote or a list (their markers aren't code).
#[derive(Clone, Debug, PartialEq)]
pub struct Fence {
    pub language: String,
    pub pieces: Vec<Range<usize>>,
}

impl Spans {
    pub fn of(&self, kind: Kind) -> &[Range<usize>] {
        &self.kinds[kind as usize]
    }

    pub fn fences(&self) -> &[Fence] {
        &self.fences
    }

    /// Tables, math and rules: markup that Live Preview leaves as written.
    pub fn as_written(&self) -> &[Range<usize>] {
        &self.written
    }
}

/// What Live Preview hides of `source` (styled as `spans`) while the lines `editing` (those
/// with the cursor or the selection) are worked on: the markup around formatted text (heading
/// `#`s, `**`, backticks, `[`…`](url)`, `>`, code fences), except on those lines and in a code
/// block they touch. Tables, math, rules, HTML and front matter stay as written, and so does a
/// link with no words. Character ranges, sorted, never across a line's end.
pub fn hidden(source: &str, spans: &Spans, editing: std::ops::RangeInclusive<usize>) -> Vec<Range<usize>> {
    let chars: Vec<char> = source.chars().collect();
    let edited = lines(&chars, &editing);
    // A code block with an edited line shows whole, fences and all.
    let blocks: Vec<&Range<usize>> = spans.of(Kind::CodeBlock).iter().filter(|block| overlap(block, &edited)).collect();
    let kept: Vec<&Range<usize>> =
        spans.as_written().iter().chain(spans.of(Kind::Html)).chain(spans.of(Kind::Frontmatter)).collect();
    let mut hidden = Vec::new();
    let markers = spans.of(Kind::Marker).iter().map(|range| (range, false));
    for (range, address) in markers.chain(spans.of(Kind::Url).iter().map(|range| (range, true))) {
        // A link with no words is all address: hiding it would leave nothing.
        let wordless = address && matches!(chars.get(range.start), Some('[' | '!'));
        if wordless || overlap(range, &edited) || blocks.iter().any(|b| overlap(range, b)) || kept.iter().any(|k| overlap(range, k)) {
            continue;
        }
        // Never a line's end: hidden, it would join two lines.
        let mut start = range.start;
        for i in range.clone() {
            if chars.get(i) == Some(&'\n') {
                if start < i {
                    hidden.push(start..i);
                }
                start = i + 1;
            }
        }
        if start < range.end {
            hidden.push(start..range.end);
        }
    }
    // A task's "- " (its checkbox takes the place), and a callout's first line (its title is
    // drawn instead).
    for marker in spans.of(Kind::ListMarker) {
        if is_bullet(&chars, marker) && task_after(&chars, marker.end).is_some() && !overlap(marker, &edited) {
            hidden.push(marker.start..marker.end + 1);
        }
    }
    for quote in spans.of(Kind::Quote) {
        if let Some((header, _)) = callout_header(&chars, quote)
            && !overlap(&header, &edited)
        {
            hidden.push(header);
        }
    }
    hidden.sort_by_key(|range| range.start);
    hidden
}

/// The backgrounds behind inline code and #tags, as `Paper::set_chips` takes them.
fn chips(spans: &Spans, colors: &crate::palettes::Colors) -> Vec<(Range<usize>, gdk::RGBA)> {
    let code = render::rgba(colors.code_background, 1.0);
    let tag = render::rgba(colors.accent, 0.14);
    let mut chips: Vec<_> = spans
        .of(Kind::Code)
        .iter()
        .map(|range| (range.clone(), code))
        .chain(spans.of(Kind::Tag).iter().map(|range| (range.clone(), tag)))
        .collect();
    chips.sort_by_key(|(range, _)| range.start);
    chips
}

/// The lines of the code fences Live Preview hides (those of blocks away from the lines
/// `editing`), each with its line's end: character ranges, sorted.
pub fn fences(source: &str, spans: &Spans, editing: std::ops::RangeInclusive<usize>) -> Vec<Range<usize>> {
    let chars: Vec<char> = source.chars().collect();
    let edited = lines(&chars, &editing);
    let is_fence = |range: Range<usize>| {
        let line: String = chars[range].iter().collect();
        let line = line.trim_start();
        line.starts_with("```") || line.starts_with("~~~")
    };
    let mut fences = Vec::new();
    for block in spans.of(Kind::CodeBlock).iter().filter(|block| !overlap(block, &edited)) {
        let Some(first_end) = (block.start..block.end).find(|&i| chars[i] == '\n') else { continue };
        if !is_fence(block.start..first_end) {
            continue;
        }
        fences.push(block.start..first_end + 1);
        let last_start = (block.start..block.end).rev().find(|&i| chars[i] == '\n').map_or(block.start, |i| i + 1);
        if last_start > first_end && is_fence(last_start..block.end) {
            fences.push(last_start..(block.end + 1).min(chars.len()));
        }
    }
    fences
}

/// What Live Preview draws in place of markup away from the lines `editing`, as `hidden` hides
/// it: a dot for a list's "-", a checkbox for a task's "[ ]" (ticked with any mark), a line for
/// a rule; the text under them stays, unseen. A callout's first line (hidden) gets its title,
/// at its start. Character ranges, sorted.
pub fn drawn(source: &str, spans: &Spans, editing: std::ops::RangeInclusive<usize>) -> Vec<(Range<usize>, Mark)> {
    let chars: Vec<char> = source.chars().collect();
    let edited = lines(&chars, &editing);
    let mut marks = Vec::new();
    for marker in spans.of(Kind::ListMarker) {
        // "1. [ ]" keeps its number and gets a checkbox too.
        match task_after(&chars, marker.end) {
            Some((task, done)) => marks.push((task, Mark::Task(done))),
            None if is_bullet(&chars, marker) => marks.push((marker.clone(), Mark::Bullet)),
            None => {}
        }
    }
    for kept in spans.as_written() {
        let line: String = chars[kept.clone()].iter().collect();
        let rule = line.trim_end_matches(['\n', '\r']);
        let first = rule.trim_start().chars().next();
        let drawn = rule.chars().filter(|c| !c.is_whitespace()).count();
        if matches!(first, Some('-' | '*' | '_')) && drawn >= 3 && rule.chars().all(|c| c.is_whitespace() || Some(c) == first) {
            marks.push((kept.start..kept.start + rule.chars().count(), Mark::Rule));
        }
    }
    for quote in spans.of(Kind::Quote) {
        if let Some((header, callout)) = callout_header(&chars, quote) {
            marks.push((header.start..header.start, Mark::Title(callout.title, callout.kind)));
        }
    }
    // Formulas, but those in a table: the table is drawn whole.
    for (range, latex) in &spans.formulas {
        if !spans.blocks.iter().any(|(block, _)| overlap(block, range)) {
            marks.push((range.clone(), Mark::Math(latex.clone())));
        }
    }
    marks.retain(|(range, _)| {
        // A title is drawn over its hidden line: whether that is edited counts.
        let line = range.start..range.end.max(range.start + 1);
        !overlap(&line, &edited)
    });
    marks.sort_by_key(|(range, _)| range.start);
    marks
}

/// The quotes in `source` (styled as `spans`), each with its callout kind if it is one: Live
/// Preview draws a bar down a quote's side and a callout on a box. Character ranges.
pub fn panels(source: &str, spans: &Spans) -> Vec<(Range<usize>, Option<&'static str>)> {
    let chars: Vec<char> = source.chars().collect();
    spans.of(Kind::Quote).iter().map(|quote| (quote.clone(), callout_header(&chars, quote).map(|(_, callout)| callout.kind))).collect()
}

/// The character range of the lines `editing` in `chars` (to the start of the next line).
fn lines(chars: &[char], editing: &std::ops::RangeInclusive<usize>) -> Range<usize> {
    let mut starts = vec![0];
    starts.extend(chars.iter().enumerate().filter(|(_, c)| **c == '\n').map(|(i, _)| i + 1));
    let line = |n: usize| starts.get(n).copied().unwrap_or(chars.len() + 1);
    line(*editing.start())..line(editing.end() + 1)
}

fn overlap(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

/// Whether a list marker is a bullet ("-", "*", "+"), not a number.
fn is_bullet(chars: &[char], marker: &Range<usize>) -> bool {
    marker.len() == 1 && matches!(chars[marker.start], '-' | '*' | '+')
}

/// The "[ ]" after a list marker ending at `at`, any mark in it, and whether it is ticked.
fn task_after(chars: &[char], at: usize) -> Option<(Range<usize>, bool)> {
    let &[' ', '[', mark, ']'] = chars.get(at..at + 4)? else { return None };
    let ends = chars.get(at + 4).is_none_or(|c| c.is_whitespace());
    (ends && mark != '\n').then_some((at + 1..at + 4, mark != ' '))
}

/// A quote's first line, if it starts a callout: where it is after its ">"s, and the callout.
fn callout_header(chars: &[char], quote: &Range<usize>) -> Option<(Range<usize>, obsidian::Callout)> {
    let end = (quote.start..quote.end).find(|&i| chars[i] == '\n').unwrap_or(quote.end);
    let start = (quote.start..end).find(|&i| !matches!(chars[i], '>' | ' ' | '\t')).unwrap_or(end);
    let line: String = chars[start..end].iter().collect();
    obsidian::callout(&line).map(|callout| (start..end, callout))
}

/// The blocks Live Preview shows drawn rather than written, away from the lines `editing`:
/// tables, formulas and pictures with lines to themselves, and Mermaid flowcharts. Character
/// ranges of whole lines (without the last one's end).
pub fn blocks(source: &str, spans: &Spans, editing: std::ops::RangeInclusive<usize>) -> Vec<(Range<usize>, Block)> {
    let chars: Vec<char> = source.chars().collect();
    let edited = lines(&chars, &editing);
    spans.blocks.iter().filter(|(range, _)| !overlap(range, &edited)).cloned().collect()
}

/// The pictures in lines that have other things in them too (a sentence, a list item), away
/// from the lines `editing`: Live Preview draws them under their line. Each line (whole,
/// without its end) with its pictures' markup; character ranges.
pub fn pictures(source: &str, spans: &Spans, editing: std::ops::RangeInclusive<usize>) -> Vec<(Range<usize>, Vec<Range<usize>>)> {
    let chars: Vec<char> = source.chars().collect();
    let edited = lines(&chars, &editing);
    let mut found: Vec<(Range<usize>, Vec<Range<usize>>)> = Vec::new();
    for picture in &spans.pictures {
        // A table draws its own.
        if spans.blocks.iter().any(|(block, _)| overlap(block, picture)) {
            continue;
        }
        let start = chars[..picture.start].iter().rposition(|&c| c == '\n').map_or(0, |i| i + 1);
        let end = chars[picture.end..].iter().position(|&c| c == '\n').map_or(chars.len(), |i| picture.end + i);
        if overlap(&(start..end), &edited) {
            continue;
        }
        match found.last_mut() {
            Some((line, pictures)) if *line == (start..end) => pictures.push(picture.clone()),
            _ => found.push((start..end, vec![picture.clone()])),
        }
    }
    found
}

/// A block Live Preview may draw (see `blocks`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Block {
    Table,
    /// `$$…$$`.
    Math,
    /// A Mermaid flowchart's fenced block.
    Chart,
    /// `![…](…)` or `![[…]]` showing a picture.
    Image,
    /// `![[Note]]` or `![[Note#Heading]]`: the note, or that part of it, shown in place.
    Embed,
}

/// Picture endings: what `![…](…)` draws as a picture.
const PICTURES: [&str; 11] = ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "avif", "ico", "tif", "tiff"];

/// What `![…](target)` shows, by its ending: a picture, a note (`.md`, or no ending, as
/// Obsidian names notes), or nothing Live Preview draws (a PDF, a page on the web).
fn embedded(target: &str) -> Option<Block> {
    let path = target.split('#').next().unwrap_or(target);
    let name = path.rsplit('/').next().unwrap_or(path);
    // A few letters or digits after the last dot: "v1.2 release" has no ending.
    let ending = name
        .rsplit_once('.')
        .map(|(_, ending)| ending.to_lowercase())
        .filter(|ending| !ending.is_empty() && ending.len() <= 5 && ending.chars().all(|c| c.is_ascii_alphanumeric()));
    match ending.as_deref() {
        Some(ending) if PICTURES.contains(&ending) => Some(Block::Image),
        _ if links::has_scheme(target) => None,
        Some("md") | None => Some(Block::Embed),
        Some(_) => None,
    }
}

/// Something Live Preview draws (see `drawn`).
#[derive(Clone, Debug, PartialEq)]
pub enum Mark {
    /// A list item's "-", "*" or "+": a dot.
    Bullet,
    /// A task's "[ ]": a checkbox, ticked or not.
    Task(bool),
    /// A rule's "---": a line across.
    Rule,
    /// A callout's first line: its title, in its kind's colour.
    Title(String, &'static str),
    /// A formula (`$…$`): drawn as the rendered view writes it (what is between the dollars).
    Math(String),
}

/// Works out how each part of `source` should look.
pub fn spans(source: &str) -> Spans {
    let mut out = Collector::default();
    // Open links and images, each with the extent of its label so far.
    let mut links: Vec<(Range<usize>, Option<Range<usize>>)> = Vec::new();
    // The fenced code block being read, if it names a language, and its code so far.
    let mut fence: Option<(String, Vec<Range<usize>>)> = None;
    // Code and math, where Obsidian's additions don't count.
    let mut code: Vec<Range<usize>> = Vec::new();
    for (event, range) in render::events(source) {
        if let Event::Code(_) | Event::InlineMath(_) | Event::DisplayMath(_) | Event::Start(Tag::CodeBlock(_)) = &event {
            code.push(range.clone());
        }
        match &event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                // "rust", "py title", "rust,ignore", "{.python}".
                let language = info.split([' ', '\t', ',']).next().unwrap_or_default().trim_matches(['{', '}', '.']);
                if language == "mermaid" {
                    out.block(source, range.clone(), Block::Chart);
                }
                fence = (!language.is_empty()).then(|| (language.to_owned(), Vec::new()));
            }
            Event::Text(_) => {
                if let Some((_, pieces)) = &mut fence {
                    pieces.push(range.clone());
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((language, pieces)) = fence.take() {
                    out.fences.push((language, pieces));
                }
            }
            _ => {}
        }
        if let Event::End(TagEnd::Link | TagEnd::Image) = event {
            if let Some((link, label)) = links.pop() {
                let label = picture_label(source, &link, label);
                link_parts(link, label, &mut out);
            }
            continue;
        }
        // Whatever sits inside a link is its label.
        if let Some((_, label)) = links.last_mut() {
            *label = Some(match label.take() {
                Some(l) => l.start.min(range.start)..l.end.max(range.end),
                None => range.clone(),
            });
        }
        match event {
            Event::Start(Tag::Image { dest_url, .. }) => {
                let block = embedded(&dest_url);
                let alone = block.is_some_and(|block| out.block(source, range.clone(), block));
                // A picture beside other things is drawn under its line (one on the web isn't).
                if block == Some(Block::Image) && !alone && !links::has_scheme(&dest_url) {
                    out.pictures.push(range.clone());
                }
                links.push((range, None));
            }
            Event::Start(Tag::Link { .. }) => links.push((range, None)),
            Event::Start(Tag::Heading { level, .. }) => heading(source, range, level, &mut out),
            Event::Start(Tag::Strong) => wrapped(Kind::Strong, range, 2, &mut out),
            Event::Start(Tag::Emphasis) => wrapped(Kind::Emphasis, range, 1, &mut out),
            Event::Start(Tag::Strikethrough) => {
                let tildes = source[range.clone()].bytes().take_while(|&b| b == b'~').count();
                wrapped(Kind::Strike, range, tildes, &mut out);
            }
            Event::Code(_) => {
                let ticks = source[range.clone()].bytes().take_while(|&b| b == b'`').count();
                wrapped(Kind::Code, range, ticks, &mut out);
            }
            Event::InlineMath(latex) => {
                out.keep(range.clone());
                out.formulas.push((range.clone(), latex.to_string()));
                wrapped(Kind::Code, range, 1, &mut out);
            }
            Event::DisplayMath(latex) => {
                out.keep(range.clone());
                // On lines of its own it is a block; in a sentence, drawn in the line.
                if !out.block(source, range.clone(), Block::Math) {
                    out.formulas.push((range.clone(), latex.to_string()));
                }
                wrapped(Kind::Code, range, 2, &mut out);
            }
            Event::Start(Tag::CodeBlock(kind)) => code_block(source, range, kind.is_fenced(), &mut out),
            Event::Start(Tag::Item) => list_marker(source, range, &mut out),
            Event::Start(Tag::BlockQuote(_)) => quote(source, range, &mut out),
            Event::Start(Tag::Table(_)) => {
                out.keep(range.clone());
                out.block(source, range.clone(), Block::Table);
                table(source, range, &mut out);
            }
            Event::Start(Tag::TableHead) => out.push(Kind::Strong, without_newline(source, range)),
            Event::Start(Tag::MetadataBlock(_)) => out.push(Kind::Frontmatter, without_newline(source, range)),
            Event::Rule => {
                out.keep(range.clone());
                out.push(Kind::Marker, without_newline(source, range));
            }
            Event::Start(Tag::HtmlBlock) | Event::InlineHtml(_) => {
                out.push(Kind::Html, without_newline(source, range));
            }
            Event::TaskListMarker(_) => out.push(Kind::Task, range),
            _ => {}
        }
    }
    // A ticked task's text, struck through as in the rendered view: any mark but a space.
    for marker in out.ranges[Kind::ListMarker as usize].clone() {
        let rest = &source[marker.end..];
        let ticked = rest.strip_prefix(" [").and_then(|rest| {
            let mut chars = rest.chars();
            let mark = chars.next()?;
            chars.as_str().strip_prefix("] ").filter(|_| !matches!(mark, ' ' | '\n'))
        });
        if let Some(text) = ticked {
            let start = source.len() - text.len();
            out.push(Kind::Strike, start..start + text.find('\n').unwrap_or(text.len()));
        }
    }
    // Obsidian's additions: highlights (their == dimmed, hidden in Live Preview like other
    // markup), tags, comments, and block ids (dimmed and hidden like markup).
    let additions = obsidian::additions_in(source, &code);
    for range in additions.highlights {
        out.push(Kind::Marker, range.start..range.start + 2);
        out.push(Kind::Marker, range.end - 2..range.end);
        out.push(Kind::Highlight, range);
    }
    for range in additions.tags {
        out.push(Kind::Tag, range);
    }
    for range in additions.comments {
        out.push(Kind::Comment, range);
    }
    for range in additions.block_ids {
        out.push(Kind::Marker, range);
    }
    out.into_spans(source)
}

/// The quote in quote style; the "> " (or "> > ") at the start of each line dimmed.
fn quote(source: &str, range: Range<usize>, out: &mut Collector) {
    let block = without_newline(source, range);
    out.push(Kind::Quote, block.clone());
    let mut line_start = block.start;
    for line in source[block.clone()].split_inclusive('\n') {
        let bytes = line.as_bytes();
        let mut i = 0;
        loop {
            while bytes.get(i) == Some(&b' ') {
                i += 1;
            }
            if bytes.get(i) != Some(&b'>') {
                break;
            }
            let marker = i;
            i += 1;
            if bytes.get(i) == Some(&b' ') {
                i += 1;
            }
            out.push(Kind::Marker, line_start + marker..line_start + i);
        }
        line_start += line.len();
    }
}

/// The pipes between cells and the "|---|" line under the header, dimmed.
fn table(source: &str, range: Range<usize>, out: &mut Collector) {
    let block = without_newline(source, range);
    let mut line_start = block.start;
    for (index, line) in source[block.clone()].split_inclusive('\n').enumerate() {
        if index == 1 {
            let divider = line.trim_end_matches(['\n', '\r']);
            out.push(Kind::Marker, line_start..line_start + divider.len());
        } else {
            for (i, _) in line.match_indices('|') {
                out.push(Kind::Marker, line_start + i..line_start + i + 1);
            }
        }
        line_start += line.len();
    }
}

/// The "-", "*", "+", "1." or "1)" that starts a list item.
fn list_marker(source: &str, item: Range<usize>, out: &mut Collector) {
    let text = &source[item.clone()];
    let indent = text.len() - text.trim_start_matches(' ').len();
    let rest = &text[indent..];
    let len = if rest.starts_with(['-', '*', '+']) {
        1
    } else {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && rest[digits..].starts_with(['.', ')']) { digits + 1 } else { 0 }
    };
    let start = item.start + indent;
    out.push(Kind::ListMarker, start..start + len);
}

/// A link's label, but for `![[pic.png|300]]`: there the size after `|` comes as the label,
/// and the picture's name is what reads as words.
fn picture_label(source: &str, link: &Range<usize>, label: Option<Range<usize>>) -> Option<Range<usize>> {
    let is_size = |text: &str| {
        let (width, height) = text.split_once('x').unwrap_or((text, "1"));
        [width, height].iter().all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    let text = &source[link.clone()];
    match text.find('|') {
        Some(bar) if text.starts_with("![[") && label.as_ref().is_some_and(|label| is_size(&source[label.clone()])) => {
            Some(link.start + 3..link.start + bar)
        }
        _ => label,
    }
}

/// "[label](url)": the label as a link, what comes before it dimmed, the rest as the URL.
fn link_parts(link: Range<usize>, label: Option<Range<usize>>, out: &mut Collector) {
    match label {
        Some(label) => {
            out.push(Kind::Marker, link.start..label.start);
            out.push(Kind::LinkText, label.clone());
            out.push(Kind::Url, label.end..link.end);
        }
        None => out.push(Kind::Url, link),
    }
}

/// The whole block in code style; for a fenced block, its opening and closing fences dimmed.
fn code_block(source: &str, range: Range<usize>, fenced: bool, out: &mut Collector) {
    let block = without_newline(source, range);
    out.push(Kind::CodeBlock, block.clone());
    if !fenced {
        return;
    }
    let text = &source[block.clone()];
    let first_line = text.find('\n').unwrap_or(text.len());
    out.push(Kind::Marker, block.start..block.start + first_line);
    if let Some(last_newline) = text.rfind('\n') {
        let last_line = text[last_newline + 1..].trim_start();
        if last_line.starts_with("```") || last_line.starts_with("~~~") {
            out.push(Kind::Marker, block.start + last_newline + 1..block.end);
        }
    }
}

/// "**bold**": the whole span in `kind`, the `len` delimiter characters at each end dimmed.
fn wrapped(kind: Kind, range: Range<usize>, len: usize, out: &mut Collector) {
    out.push(kind, range.clone());
    out.push(Kind::Marker, range.start..(range.start + len).min(range.end));
    out.push(Kind::Marker, range.end.saturating_sub(len).max(range.start)..range.end);
}

/// "## Title": the line in its level's style, the hashes and the space after them dimmed.
/// Also dims closing hashes ("# Title #") and the "===" / "---" under a two-line heading.
fn heading(source: &str, range: Range<usize>, level: HeadingLevel, out: &mut Collector) {
    let line = without_newline(source, range);
    out.push(heading_kind(level), line.clone());
    let text = &source[line.clone()];
    let indent = text.len() - text.trim_start_matches(' ').len();
    let hashes = text[indent..].bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 {
        if let Some(newline) = text.rfind('\n') {
            out.push(Kind::Marker, line.start + newline + 1..line.end);
        }
        return;
    }
    let spaces = text[indent + hashes..].bytes().take_while(|&b| b == b' ' || b == b'\t').count();
    out.push(Kind::Marker, line.start + indent..line.start + indent + hashes + spaces);
    // Trailing hashes close the heading only when a space separates them from the title.
    let body = text.trim_end();
    let closing = body.bytes().rev().take_while(|&b| b == b'#').count();
    let before = body.len() - closing;
    if closing > 0 && before > indent + hashes && body.as_bytes()[before - 1] == b' ' {
        out.push(Kind::Marker, line.start + before..line.start + body.len());
    }
}

fn heading_kind(level: HeadingLevel) -> Kind {
    match level {
        HeadingLevel::H1 => Kind::H1,
        HeadingLevel::H2 => Kind::H2,
        HeadingLevel::H3 => Kind::H3,
        HeadingLevel::H4 => Kind::H4,
        HeadingLevel::H5 => Kind::H5,
        HeadingLevel::H6 => Kind::H6,
    }
}

fn without_newline(source: &str, range: Range<usize>) -> Range<usize> {
    let text = &source[range.clone()];
    range.start..range.start + text.trim_end_matches(['\n', '\r']).len()
}

/// Byte ranges per kind, collected while walking the parse.
#[derive(Default)]
struct Collector {
    ranges: [Vec<Range<usize>>; KINDS],
    /// Fenced code: language and byte ranges.
    fences: Vec<(String, Vec<Range<usize>>)>,
    /// Tables, math and rules (byte ranges): left as written in Live Preview.
    written: Vec<Range<usize>>,
    /// Blocks Live Preview may draw: whole lines (byte ranges).
    blocks: Vec<(Range<usize>, Block)>,
    /// Pictures sharing their line with other things (byte ranges).
    pictures: Vec<Range<usize>>,
    /// Formulas in lines with other things, and what is between their dollars (byte ranges).
    formulas: Vec<(Range<usize>, String)>,
}

impl Collector {
    fn push(&mut self, kind: Kind, range: Range<usize>) {
        if range.start < range.end {
            self.ranges[kind as usize].push(range);
        }
    }

    fn keep(&mut self, range: Range<usize>) {
        self.written.push(range);
    }

    /// `range` as a block Live Preview may draw, if it has its lines to itself (nothing
    /// beside it, not in a quote or a list): whether it does.
    fn block(&mut self, source: &str, range: Range<usize>, kind: Block) -> bool {
        let range = without_newline(source, range);
        let start = source[..range.start].rfind('\n').map_or(0, |i| i + 1);
        let end = source[range.end..].find('\n').map_or(source.len(), |i| range.end + i);
        let alone = source[start..range.start].trim().is_empty() && source[range.end..end].trim().is_empty();
        if alone {
            self.blocks.push((start..end, kind));
        }
        alone
    }

    /// Character ranges, sorted, with overlapping or touching ones merged.
    fn into_spans(self, source: &str) -> Spans {
        let starts: Vec<usize> = source.char_indices().map(|(i, _)| i).collect();
        let char_at = |byte: usize| starts.partition_point(|&start| start < byte);
        let fences = self
            .fences
            .into_iter()
            .map(|(language, pieces)| Fence { language, pieces: pieces.into_iter().map(|p| char_at(p.start)..char_at(p.end)).collect() })
            .collect();
        let ranges = self.ranges.map(|mut ranges| {
            ranges.sort_by_key(|r| r.start);
            let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
            for range in ranges {
                let range = char_at(range.start)..char_at(range.end);
                match merged.last_mut() {
                    Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                    _ => merged.push(range),
                }
            }
            merged
        });
        let written = self.written.into_iter().map(|r| char_at(r.start)..char_at(r.end)).collect();
        let blocks = self.blocks.into_iter().map(|(r, block)| (char_at(r.start)..char_at(r.end), block)).collect();
        let pictures = self.pictures.into_iter().map(|r| char_at(r.start)..char_at(r.end)).collect();
        let formulas = self.formulas.into_iter().map(|(r, latex)| (char_at(r.start)..char_at(r.end), latex)).collect();
        Spans { kinds: ranges, fences, written, blocks, pictures, formulas }
    }
}

#[cfg(test)]
// `&[0..8]` here is meant: a list holding one range.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    #[test]
    fn heading_gets_its_level_and_dims_the_hashes() {
        let s = spans("## Title\n");
        assert_eq!(s.of(Kind::H2), &[0..8]);
        assert_eq!(s.of(Kind::Marker), &[0..3]);
    }

    #[test]
    fn bold_text_is_bold_and_its_stars_are_dimmed() {
        let s = spans("a **bold** b");
        assert_eq!(s.of(Kind::Strong), &[2..10]);
        assert_eq!(s.of(Kind::Marker), &[2..4, 8..10]);
    }

    #[test]
    fn italic_and_struck_text_dim_their_delimiters() {
        let s = spans("*it* ~~no~~");
        assert_eq!(s.of(Kind::Emphasis), &[0..4]);
        assert_eq!(s.of(Kind::Strike), &[5..11]);
        assert_eq!(s.of(Kind::Marker), &[0..1, 3..4, 5..7, 9..11]);
    }

    #[test]
    fn inline_code_dims_its_backticks_however_many() {
        let s = spans("use `ls -l` now");
        assert_eq!(s.of(Kind::Code), &[4..11]);
        assert_eq!(s.of(Kind::Marker), &[4..5, 10..11]);

        let s = spans("``a`b``");
        assert_eq!(s.of(Kind::Code), &[0..7]);
        assert_eq!(s.of(Kind::Marker), &[0..2, 5..7]);
    }

    #[test]
    fn math_is_styled_like_code_with_its_dollars_dimmed() {
        let s = spans("so $x^2$ and $$y$$");
        assert_eq!(s.of(Kind::Code), &[3..8, 13..18]);
        assert_eq!(s.of(Kind::Marker), &[3..4, 7..8, 13..15, 16..18]);
    }

    fn fence(language: &str, pieces: &[Range<usize>]) -> Fence {
        Fence { language: language.to_owned(), pieces: pieces.to_vec() }
    }

    #[test]
    fn fenced_code_names_its_language_and_where_its_code_is() {
        // "```rust\n" starts at 5, so the code "fn main() {}\n" is 13..26; "py title" is py.
        let s = spans("Text\n```rust\nfn main() {}\n```\n\n~~~py title\nx = 1\n~~~\n```\nno language\n```\n");
        assert_eq!(s.fences(), &[fence("rust", &[13..26]), fence("py", &[43..49])]);
        // In a quote, each line's code without its "> ".
        let s = spans("> ```sh\n> ls -l\n> echo hi\n> ```\n");
        assert_eq!(s.fences(), &[fence("sh", &[10..16, 18..26])]);
    }

    #[test]
    fn fenced_code_block_is_one_block_with_dimmed_fences() {
        let s = spans("```rust\nfn main() {}\n```\n");
        assert_eq!(s.of(Kind::CodeBlock), &[0..24]);
        assert_eq!(s.of(Kind::Marker), &[0..7, 21..24]);
    }

    #[test]
    fn link_label_stands_out_and_the_url_is_dimmed() {
        let s = spans("see [docs](https://x.io) ok");
        assert_eq!(s.of(Kind::LinkText), &[5..9]);
        assert_eq!(s.of(Kind::Marker), &[4..5]);
        assert_eq!(s.of(Kind::Url), &[9..24]);
    }

    #[test]
    fn wikilinks_autolinks_images_and_formatted_labels_split_the_same_way() {
        let s = spans("[[Other Note]]");
        assert_eq!(s.of(Kind::Marker), &[0..2]);
        assert_eq!(s.of(Kind::LinkText), &[2..12]);
        assert_eq!(s.of(Kind::Url), &[12..14]);

        let s = spans("<https://a.b>");
        assert_eq!(s.of(Kind::Marker), &[0..1]);
        assert_eq!(s.of(Kind::LinkText), &[1..12]);
        assert_eq!(s.of(Kind::Url), &[12..13]);

        let s = spans("![alt](p.png)");
        assert_eq!(s.of(Kind::Marker), &[0..2]);
        assert_eq!(s.of(Kind::LinkText), &[2..5]);
        assert_eq!(s.of(Kind::Url), &[5..13]);

        let s = spans("[**b**](u)");
        assert_eq!(s.of(Kind::LinkText), &[1..6]);
        assert_eq!(s.of(Kind::Strong), &[1..6]);
        assert_eq!(s.of(Kind::Marker), &[0..3, 4..6]);
        assert_eq!(s.of(Kind::Url), &[6..10]);
    }

    #[test]
    fn list_bullets_numbers_and_task_boxes_are_marked() {
        let s = spans("- [ ] buy milk\n1. first\n");
        assert_eq!(s.of(Kind::ListMarker), &[0..1, 15..17]);
        assert_eq!(s.of(Kind::Task), &[2..5]);
    }

    #[test]
    fn a_ticked_tasks_text_is_struck_through_whatever_its_mark() {
        let s = spans("- [x] done\n- [ ] open\n- [?] asked\n");
        assert_eq!(s.of(Kind::Strike), &[6..10, 28..33]);
    }

    #[test]
    fn quote_lines_are_quoted_and_their_markers_dimmed() {
        let s = spans("> hello\n> world\n");
        assert_eq!(s.of(Kind::Quote), &[0..15]);
        assert_eq!(s.of(Kind::Marker), &[0..2, 8..10]);
    }

    #[test]
    fn table_header_is_bold_and_pipes_and_divider_are_dimmed() {
        let s = spans("| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert_eq!(s.of(Kind::Strong), &[0..9]);
        assert_eq!(s.of(Kind::Marker), &[0..1, 4..5, 8..9, 10..19, 20..21, 24..25, 28..29]);
    }

    #[test]
    fn frontmatter_is_one_dimmed_block() {
        let s = spans("---\ntitle: x\n---\n\nbody\n");
        assert_eq!(s.of(Kind::Frontmatter), &[0..16]);
    }

    #[test]
    fn a_rule_further_down_is_a_rule_even_with_text_right_under_it() {
        // Front matter is only ever at the top; this `---` and the next are rules.
        let s = spans("a\n\n---\n# Plans\n---\n");
        assert!(s.of(Kind::Frontmatter).is_empty());
        assert_eq!(s.of(Kind::H1), &[7..14]);
        assert_eq!(s.of(Kind::Marker), &[3..6, 7..9, 15..18]);
    }

    #[test]
    fn live_preview_hides_markup_except_on_the_lines_being_edited() {
        let text = "# Title\nSome **bold** and `code`.\n\nlast";
        let s = spans(text);
        assert_eq!(hidden(text, &s, 3..=3), vec![0..2, 13..15, 19..21, 26..27, 31..32]);
        assert_eq!(hidden(text, &s, 1..=1), vec![0..2]);
        assert_eq!(hidden(text, &s, 0..=1), Vec::<Range<usize>>::new());
    }

    #[test]
    fn live_preview_keeps_a_links_words_and_the_address_of_a_link_without_words() {
        let text = "[the plan](Plans.md) [[Note|alias]] [](x.md)";
        let s = spans(text);
        assert_eq!(hidden(text, &s, 5..=5), vec![0..1, 9..20, 21..28, 33..35]);
    }

    #[test]
    fn live_preview_shows_a_code_block_being_edited_and_leaves_tables_math_and_rules() {
        let text = "```rust\nlet x = 1;\n```\n\n| a | b |\n|---|---|\n\n$x^2$ and more\n\n---\n";
        let s = spans(text);
        assert_eq!(hidden(text, &s, 9..=9), vec![0..7, 19..22]);
        assert_eq!(hidden(text, &s, 1..=1), Vec::<Range<usize>>::new());
    }

    #[test]
    fn live_preview_shrinks_the_fence_lines_of_code_blocks_away_from_the_cursor() {
        let text = "```rust\nlet x = 1;\n```\n\n    indented\n\n~~~\nlast\n~~~";
        let s = spans(text);
        // Each fence with its line's end; the last one has none. Indented code has no fences.
        assert_eq!(fences(text, &s, 3..=3), vec![0..8, 19..23, 38..42, 47..50]);
        assert_eq!(fences(text, &s, 1..=1), vec![38..42, 47..50]);
    }

    /// `text` without what `hidden` hides.
    fn shown(text: &str, hidden: &[Range<usize>]) -> String {
        text.chars().enumerate().filter(|(i, _)| !hidden.iter().any(|h| h.contains(i))).map(|(_, c)| c).collect()
    }

    #[test]
    fn live_preview_draws_bullets_checkboxes_and_rules_away_from_the_cursor() {
        let text = "- one\n- [ ] two\n- [x] three\n- [?] four\n\n---\n\n1. five\n* six";
        let s = spans(text);
        assert_eq!(
            drawn(text, &s, 1..=1),
            vec![(0..1, Mark::Bullet), (18..21, Mark::Task(true)), (30..33, Mark::Task(true)), (40..43, Mark::Rule), (53..54, Mark::Bullet)]
        );
        // A task's checkbox stands where its "-" was.
        assert_eq!(shown(text, &hidden(text, &s, 1..=1)), "- one\n- [ ] two\n[x] three\n[?] four\n\n---\n\n1. five\n* six");
    }

    #[test]
    fn live_preview_draws_a_callouts_title_over_its_first_line_and_quotes_on_panels() {
        let text = "- [ ] a\n- [ ] b\n\n> [!tip] Title\n> body\n\n> [!NOTE]\n> GitHub's kind\n\n> plain";
        let s = spans(text);
        assert_eq!(shown(text, &hidden(text, &s, 1..=1)), "[ ] a\n- [ ] b\n\n\nbody\n\n\nGitHub's kind\n\nplain");
        assert_eq!(
            drawn(text, &s, 1..=1),
            vec![(2..5, Mark::Task(false)), (19..19, Mark::Title("Title".into(), "tip")), (42..42, Mark::Title("Note".into(), "note"))]
        );
        assert_eq!(panels(text, &s), vec![(17..38, Some("tip")), (40..65, Some("note")), (67..74, None)]);
    }

    #[test]
    fn live_preview_draws_tables_pictures_flowcharts_and_formulas_with_lines_to_themselves() {
        let text = "Text\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n![[pic.png|300]]\n\nSee ![x](inline.png) here.\n\n$$\nx^2\n$$\n\n```mermaid\ngraph TD\nA-->B\n```\n\n> | q |\n> |---|\n";
        let s = spans(text);
        let drawn = |editing| blocks(text, &s, editing).into_iter().map(|(range, block)| (&text[range], block)).collect::<Vec<_>>();
        assert_eq!(
            drawn(0..=0),
            vec![
                ("| a | b |\n|---|---|\n| 1 | 2 |", Block::Table),
                ("![[pic.png|300]]", Block::Image),
                ("$$\nx^2\n$$", Block::Math),
                ("```mermaid\ngraph TD\nA-->B\n```", Block::Chart),
            ]
        );
        // With the cursor in it, the table is written out.
        assert_eq!(drawn(3..=3).len(), 3);
    }

    #[test]
    fn live_preview_draws_an_embedded_note_but_not_other_files() {
        let text = "![[Other note]]\n\n![[Plans#Budget|the budget]]\n\n![](Notes/Other.md)\n\n![[paper.pdf]]\n\n![[v1.2 release]]\n\n![](https://example.com/page)\n";
        let s = spans(text);
        let drawn: Vec<_> = blocks(text, &s, 99..=99).into_iter().map(|(range, block)| (&text[range], block)).collect();
        assert_eq!(
            drawn,
            vec![
                ("![[Other note]]", Block::Embed),
                ("![[Plans#Budget|the budget]]", Block::Embed),
                ("![](Notes/Other.md)", Block::Embed),
                ("![[v1.2 release]]", Block::Embed),
            ]
        );
    }

    #[test]
    fn live_preview_draws_pictures_in_a_line_under_it() {
        let text = "Text ![a](one.png) and ![[two.jpg|40]] here.\n- ![b](three.png)\n![alone](four.png)\n\n| ![c](cell.png) |\n|---|\n\nSee ![x](https://example.com/p.png) and ![[Note]].\n";
        let s = spans(text);
        let found = |editing| {
            pictures(text, &s, editing)
                .into_iter()
                .map(|(line, pictures)| (&text[line], pictures.into_iter().map(|p| &text[p]).collect::<Vec<_>>()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            found(99..=99),
            vec![
                ("Text ![a](one.png) and ![[two.jpg|40]] here.", vec!["![a](one.png)", "![[two.jpg|40]]"]),
                ("- ![b](three.png)", vec!["![b](three.png)"]),
            ]
        );
        // The line being edited shows its markup instead.
        assert_eq!(found(0..=0).len(), 1);
    }

    #[test]
    fn a_wikilink_pictures_size_is_markup_and_its_name_the_words() {
        let s = spans("![[pic.png|300]] ![[shot.png|300x200]] ![[Note|alias]]");
        assert_eq!(s.of(Kind::LinkText), &[3..10, 20..28, 47..52]);
        assert_eq!(s.of(Kind::Url), &[10..16, 28..38, 52..54]);
    }

    #[test]
    fn live_preview_draws_formulas_in_a_line_away_from_the_cursor() {
        let text = "Area $\\pi r^2$ and $$x_1$$ here.\n\n$$\ny\n$$\n\n`$code$` and $z$";
        let s = spans(text);
        let formulas = |editing| {
            drawn(text, &s, editing)
                .into_iter()
                .filter_map(|(range, mark)| match mark {
                    Mark::Math(latex) => Some((&text[range], latex)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            formulas(99..=99),
            vec![("$\\pi r^2$", "\\pi r^2".to_owned()), ("$$x_1$$", "x_1".to_owned()), ("$z$", "z".to_owned())]
        );
        // The line being edited shows its formulas written.
        assert_eq!(formulas(0..=0).len(), 1);
    }

    #[test]
    fn rules_and_html_are_dimmed() {
        let s = spans("a\n\n---\n\nb <br> c\n");
        assert_eq!(s.of(Kind::Marker), &[3..6]);
        assert_eq!(s.of(Kind::Html), &[10..14]);
    }

    #[test]
    fn ranges_count_characters_so_persian_text_lines_up() {
        let s = spans("## سلام\n**مهم** و `کد`");
        assert_eq!(s.of(Kind::H2), &[0..7]);
        assert_eq!(s.of(Kind::Strong), &[8..15]);
        assert_eq!(s.of(Kind::Code), &[18..22]);
        assert_eq!(s.of(Kind::Marker), &[0..3, 8..10, 13..15, 18..19, 21..22]);
    }

    #[test]
    fn closing_hashes_and_underlines_are_dimmed_but_a_hash_in_the_title_is_not() {
        let s = spans("# Title #\n");
        assert_eq!(s.of(Kind::Marker), &[0..2, 8..9]);

        let s = spans("# C#\n");
        assert_eq!(s.of(Kind::Marker), &[0..2]);

        let s = spans("Title\n=====\n");
        assert_eq!(s.of(Kind::H1), &[0..11]);
        assert_eq!(s.of(Kind::Marker), &[6..11]);
    }
}
