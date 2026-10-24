//! Read mode: Markdown drawn as formatted text, with the markup itself hidden.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gtk::{gdk, gio, glib, pango, prelude::*};
use pulldown_cmark::{Alignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd};
use sourceview::prelude::*;

use crate::{classes, flowchart, gantt, links, math, obsidian, sequence, settings, state};
use crate::theme::{self, fonts};

/// What a click on the rendered text does.
#[derive(Clone)]
pub enum Target {
    /// A URL, a path relative to the file, or a `[[wiki]]` page name.
    Link(String),
    /// A task checkbox: the byte range of its `[ ]` / `[x]` in the Markdown source.
    Task(Range<usize>),
    /// A note, by its file (the notes linking here, under the rendered note).
    File(gio::File),
    /// A tag (`#travel`): clicking searches for it.
    Tag(String),
    /// A foldable callout's header: clicking shows or hides the body this tag hides.
    Fold(gtk::TextTag),
    /// A code block: clicking copies its code, as written in the note.
    Copy(Rc<str>),
}

/// Markdown features nevis understands, shared by the read view and the source styling.
pub const OPTIONS: Options = Options::ENABLE_TABLES
    .union(Options::ENABLE_FOOTNOTES)
    .union(Options::ENABLE_STRIKETHROUGH)
    .union(Options::ENABLE_TASKLISTS)
    .union(Options::ENABLE_GFM)
    .union(Options::ENABLE_WIKILINKS)
    .union(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS)
    .union(Options::ENABLE_MATH);

/// The Markdown's events, with where each is in `source`. Front matter counts only at the very
/// top of a note, as in Obsidian: pulldown-cmark takes any `---` line after a blank one, with
/// text right under it, for the start of some, and the rest of the note goes with it.
pub fn events(source: &str) -> impl Iterator<Item = (Event<'_>, Range<usize>)> {
    let top = front_matter(source);
    let rest = OPTIONS.difference(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    Parser::new_ext(&source[..top], OPTIONS).into_offset_iter().chain(
        Parser::new_ext(&source[top..], rest)
            .into_offset_iter()
            .map(move |(event, range)| (event, range.start + top..range.end + top)),
    )
}

/// How long the note's front matter is, in bytes: from a first line of `---` through the `---`
/// or `...` line that closes it; 0 when the note has none.
pub fn front_matter(source: &str) -> usize {
    let mut lines = source.split_inclusive('\n');
    let Some(first) = lines.next().filter(|line| line.trim_end() == "---") else { return 0 };
    let mut length = first.len();
    for line in lines {
        length += line.len();
        if matches!(line.trim_end(), "---" | "...") {
            return length;
        }
    }
    0
}

/// Side margins of the read view at full width (centred text has wider ones). Tag margins
/// replace the view's, so indents add to the margin.
pub const MARGIN: i32 = 28;

pub struct Rendered {
    pub buffer: sourceview::Buffer,
    /// Clickable spans; each has its own tag.
    pub targets: Vec<(gtk::TextTag, Target)>,
    /// Images and tables, to attach once `buffer` is on a view.
    pub children: Vec<(gtk::TextChildAnchor, gtk::Widget)>,
    /// Each heading's text and where it starts, for `#heading` links.
    pub headings: Vec<(String, gtk::TextMark)>,
    /// Where each block starts, by the line of the note it starts on: for going to a line
    /// (a `#^block` link) in the rendered view.
    pub lines: Vec<(usize, gtk::TextMark)>,
    /// Fenced code blocks that name a language, to colour with `highlight_code`.
    pub code: Vec<CodeBlock>,
    /// Blocks drawn on panels, outer ones first, for the view to draw behind the text.
    pub panels: Vec<Panel>,
    /// What the view draws over the text (tasks' boxes), as character ranges.
    pub marks: Vec<(Range<usize>, crate::style::Mark)>,
    /// Rounded backgrounds behind inline code and #tags, as character ranges.
    pub chips: Vec<(Range<usize>, gdk::RGBA)>,
}

/// A block drawn on a panel behind its text (by `paper::Paper`): from `start`'s line to
/// `end`'s, `inset` pixels in from the text's edges (the view's margins; into them if less
/// than 0).
pub struct Panel {
    pub start: gtk::TextMark,
    pub end: gtk::TextMark,
    pub inset: i32,
    pub look: Look,
    /// Right-to-left: a bar goes down the right side.
    pub rtl: bool,
    /// A foldable callout's: the tag hiding its body, and where the body starts.
    pub fold: Option<(gtk::TextTag, gtk::TextMark)>,
}

pub enum Look {
    /// A box filled with this colour: a callout.
    Box(gdk::RGBA),
    /// A bar in this colour down its side: a quote, an embedded note.
    Bar(gdk::RGBA),
}

/// A block open on a panel, and what to go back to at its end.
struct Open {
    panel: usize,
    blocks: usize,
    left: i32,
    list_base: usize,
    frame: Option<gtk::TextTag>,
    /// A foldable callout: where its body starts and the tag that hides it.
    fold: Option<(i32, gtk::TextTag)>,
}

pub struct CodeBlock {
    pub start: gtk::TextMark,
    pub end: gtk::TextMark,
    pub language: String,
}

/// Renders `source` into a new buffer. `folder` resolves relative image paths; `find` finds the
/// file a link or embed names (in the vault, as Obsidian does), if there is one; `text` lays a
/// text out in the view's font (list bullets hang by their width; diagrams use it too);
/// `margin` is the view's side margin, which indented blocks add to.
pub fn render(
    source: &str,
    folder: Option<&gio::File>,
    find: &dyn Fn(&str) -> Option<gio::File>,
    scheme: Option<&sourceview::StyleScheme>,
    text: &dyn Fn(&str) -> pango::Layout,
    margin: i32,
) -> Rendered {
    let buffer = sourceview::Buffer::new(None);
    buffer.set_style_scheme(scheme);
    buffer.set_highlight_syntax(false);
    let (shown, additions) = prepare(source);
    let mut r = Renderer::new(folder, find, buffer, text, margin);
    r.read(&shown, additions);
    for (event, range) in events(&shown) {
        r.event(event, range);
    }
    r.finish()
}

struct Styles {
    headings: [gtk::TextTag; 6],
    paragraph: gtk::TextTag,
    strong: gtk::TextTag,
    emphasis: gtk::TextTag,
    strike: gtk::TextTag,
    code: gtk::TextTag,
    code_block: gtk::TextTag,
    code_top: gtk::TextTag,
    code_bottom: gtk::TextTag,
    quote: gtk::TextTag,
    highlight: gtk::TextTag,
    done: gtk::TextTag,
    dim: gtk::TextTag,
    rule: gtk::TextTag,
    gap: gtk::TextTag,
    /// A diagram's line: centred.
    figure: gtk::TextTag,
    math: gtk::TextTag,
    display_math: gtk::TextTag,
}

impl Styles {
    fn new(table: &gtk::TextTagTable, margin: i32) -> Self {
        let add = |tag: gtk::TextTag| {
            table.add(&tag);
            tag
        };
        let heading = |scale: f64, above: i32, below: i32, dim: bool| {
            let colors = theme::colors();
            add(gtk::TextTag::builder()
                .weight(650)
                .scale(scale)
                .foreground(if dim { colors.dim } else { colors.heading }.to_hex())
                .pixels_above_lines(above)
                .pixels_below_lines(below)
                .build())
        };
        Self {
            headings: [
                heading(1.6, 28, 8, false),
                heading(1.3, 24, 6, false),
                heading(1.12, 18, 4, false),
                heading(1.0, 14, 4, false),
                heading(1.0, 12, 4, true),
                heading(0.9, 12, 4, true),
            ],
            paragraph: add(gtk::TextTag::builder().pixels_below_lines(10).build()),
            strong: add(gtk::TextTag::builder().weight(700).build()),
            emphasis: add(gtk::TextTag::builder().style(pango::Style::Italic).build()),
            strike: add(gtk::TextTag::builder().strikethrough(true).build()),
            // Its background is a chip the view draws (see `Paper::set_chips`).
            code: add(gtk::TextTag::builder().family(fonts::MONO).foreground(theme::colors().code.to_hex()).build()),
            // Its background is a panel (see `Panel`): a paragraph background leaves out blank
            // lines, splitting a block in two.
            code_block: add(gtk::TextTag::builder()
                .family(fonts::MONO)
                .foreground(theme::colors().code.to_hex())
                .direction(gtk::TextDirection::Ltr)
                .left_margin(margin + 12)
                .right_margin(margin + 12)
                .build()),
            code_top: add(gtk::TextTag::builder().pixels_above_lines(8).build()),
            code_bottom: add(gtk::TextTag::builder().pixels_below_lines(14).build()),
            quote: add(gtk::TextTag::builder().foreground(theme::colors().quote.to_hex()).build()),
            highlight: add(gtk::TextTag::builder().background_rgba(&{
                let c = theme::colors().changed;
                gdk::RGBA::new(c.r as f32, c.g as f32, c.b as f32, 0.32)
            }).build()),
            done: add(gtk::TextTag::builder().strikethrough(true).foreground(theme::colors().dim.to_hex()).build()),
            dim: add(gtk::TextTag::builder().foreground(theme::colors().dim.to_hex()).build()),
            rule: add(gtk::TextTag::builder().size_points(1.0).paragraph_background(theme::colors().rule.to_hex()).build()),
            figure: add(gtk::TextTag::builder().justification(gtk::Justification::Center).pixels_above_lines(4).build()),
            math: add(gtk::TextTag::builder().family(fonts::MATH).build()),
            display_math: add(gtk::TextTag::builder()
                .justification(gtk::Justification::Center)
                .direction(gtk::TextDirection::Ltr)
                .scale(1.15)
                .pixels_above_lines(4)
                .pixels_below_lines(12)
                .build()),
            gap: add(gtk::TextTag::builder().size_points(5.0).build()),
        }
    }
}

struct List {
    /// Next number for ordered lists.
    next: Option<u64>,
}

/// A table being collected.
pub struct Table {
    pub aligns: Vec<Alignment>,
    /// Each row: whether it is the header, and its cells.
    pub rows: Vec<(bool, Vec<Cell>)>,
    head: bool,
}

#[derive(Default)]
pub struct Cell {
    /// The cell's text as Pango markup, and as plain text.
    pub markup: String,
    pub plain: String,
}

struct Renderer<'m> {
    folder: Option<gio::File>,
    /// The file a link or embed names, if there is one.
    find: &'m dyn Fn(&str) -> Option<gio::File>,
    /// The note as parsed (comments and block ids blanked), and Obsidian's highlights and tags
    /// in it, as byte ranges.
    source: &'m str,
    highlights: Vec<Range<usize>>,
    tags: Vec<Range<usize>>,
    /// What was blanked (comments, block ids): left out, with a space next to it.
    gone: Vec<Range<usize>>,
    /// Text before this byte was drawn already (a task's own mark, `[?]`).
    drawn_to: usize,
    /// How many embedded notes deep this is (`![[Note]]` in a note: 1).
    depth: u8,
    /// Blocks drawn on panels (see `Panel`), outer ones first, and those open now.
    panels: Vec<Panel>,
    marks: Vec<(Range<usize>, crate::style::Mark)>,
    chips: Vec<(Range<usize>, gdk::RGBA)>,
    open: Vec<Open>,
    /// Where text starts, from both sides of the view: the margin, further in on panels.
    left: i32,
    /// Lists outside the innermost panel's block: list depth counts from there.
    list_base: usize,
    /// The innermost panel's margins, under every other tag (lists, code keep their own).
    frame: Option<gtk::TextTag>,
    /// Those margins, by `left`.
    frames: HashMap<i32, gtk::TextTag>,
    /// A callout's first line (`[!kind] Title`), drawn as its header instead.
    skipping: bool,
    /// Front matter being read, to show as properties.
    properties: Option<String>,
    /// Single line breaks join their lines, as on GitHub (strict line breaks).
    joined: bool,
    text: &'m dyn Fn(&str) -> pango::Layout,
    /// The view's side margin, which indents add to.
    margin: i32,
    /// The margin of the view itself (an embedded note's `margin` is further in): panels are
    /// placed from it.
    origin: i32,
    buffer: sourceview::Buffer,
    styles: Styles,
    /// Tags for everything inserted now: block-level ones, then inline ones.
    block: Vec<gtk::TextTag>,
    inline: Vec<gtk::TextTag>,
    lists: Vec<List>,
    quote_depth: usize,
    /// A list item started and its bullet is not written yet.
    pending_bullet: bool,
    /// Where each open list item's indent tag sits in `block`, to swap in its hanging indent.
    items: Vec<usize>,
    /// List indents by (where text starts, depth, hanging width, right-to-left).
    indents: HashMap<(i32, usize, i32, bool), gtk::TextTag>,
    code_start: Option<i32>,
    /// The code block's code so far, as written in the note (what a click on it copies).
    code_text: Option<String>,
    code_language: Option<String>,
    code: Vec<CodeBlock>,
    /// The heading being written: where it starts and its text so far.
    heading: Option<(i32, String)>,
    headings: Vec<(String, gtk::TextMark)>,
    lines: Vec<(usize, gtk::TextMark)>,
    /// Where each line of the note starts (bytes), to tell the line a block starts on.
    line_starts: Vec<usize>,
    /// Image URL and alt text, while inside `![…](…)`.
    image: Option<(String, String)>,
    table: Option<Table>,
    /// Closing Pango tags for inline styles inside a table cell.
    markup_close: Vec<&'static str>,
    targets: Vec<(gtk::TextTag, Target)>,
    children: Vec<(gtk::TextChildAnchor, gtk::Widget)>,
    /// Raised or lowered, smaller text for formulas' scripts, by rise and depth.
    scripts: HashMap<(i8, u8), gtk::TextTag>,
    /// The source of the Mermaid block being read, kept aside to draw at its end.
    mermaid: Option<String>,
    /// Tables kept as they are (for Live Preview to draw), not made into widgets.
    tables: Option<Vec<Table>>,
}

impl<'m> Renderer<'m> {
    fn new(
        folder: Option<&gio::File>,
        find: &'m dyn Fn(&str) -> Option<gio::File>,
        buffer: sourceview::Buffer,
        text: &'m dyn Fn(&str) -> pango::Layout,
        margin: i32,
    ) -> Self {
        let styles = Styles::new(&buffer.tag_table(), margin);
        Self {
            folder: folder.cloned(),
            find,
            source: "",
            highlights: Vec::new(),
            tags: Vec::new(),
            gone: Vec::new(),
            drawn_to: 0,
            depth: 0,
            panels: Vec::new(),
            marks: Vec::new(),
            chips: Vec::new(),
            open: Vec::new(),
            left: margin,
            origin: margin,
            list_base: 0,
            frame: None,
            frames: HashMap::new(),
            skipping: false,
            properties: None,
            joined: settings::get().strict_line_breaks,
            text,
            margin,
            buffer,
            styles,
            block: Vec::new(),
            inline: Vec::new(),
            lists: Vec::new(),
            quote_depth: 0,
            pending_bullet: false,
            items: Vec::new(),
            indents: HashMap::new(),
            code_start: None,
            code_text: None,
            code_language: None,
            code: Vec::new(),
            heading: None,
            headings: Vec::new(),
            lines: Vec::new(),
            line_starts: Vec::new(),
            image: None,
            table: None,
            markup_close: Vec::new(),
            targets: Vec::new(),
            children: Vec::new(),
            scripts: HashMap::new(),
            mermaid: None,
            tables: None,
        }
    }

    /// `text`'s width in pixels in the view's font.
    fn measure(&self, text: &str) -> i32 {
        (self.text)(text).pixel_size().0
    }

    fn event(&mut self, event: Event, range: Range<usize>) {
        // A callout's first line is drawn as its header, not as text.
        let leaf = matches!(event, Event::Text(_) | Event::Code(_) | Event::InlineMath(_) | Event::InlineHtml(_) | Event::FootnoteReference(_));
        if self.skipping && leaf {
            return;
        }
        match event {
            Event::Start(tag) => {
                let block = matches!(
                    tag,
                    Tag::Paragraph | Tag::Heading { .. } | Tag::Item | Tag::CodeBlock(_) | Tag::Table(_) | Tag::BlockQuote(_) | Tag::HtmlBlock
                );
                let start = range.start;
                self.start(tag, range);
                if block {
                    self.mark_line(start);
                }
            }
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text_at(&text, range),
            // U+2068…U+2069 isolate the span: `~/notes/a.md` inside Persian stays in order.
            Event::Code(text) => {
                let colors = theme::colors();
                self.chip(&format!("\u{2068}{text}\u{2069}"), self.styles.code.clone(), "<tt>", "</tt>", rgba(colors.code_background, 1.0));
            }
            Event::InlineMath(latex) => self.math(&latex, false),
            Event::DisplayMath(latex) => self.math(&latex, true),
            Event::Html(html) | Event::InlineHtml(html) => {
                let tag = html.trim().to_ascii_lowercase();
                if matches!(tag.as_str(), "<br>" | "<br/>" | "<br />") {
                    self.text("\u{2028}");
                } else if !tag.starts_with("<!--") {
                    self.styled(&html, self.styles.dim.clone(), "", "");
                }
            }
            Event::FootnoteReference(label) => {
                self.styled(&format!("[{label}]"), self.styles.dim.clone(), "", "")
            }
            // A callout's header line ends here; what follows is its body.
            Event::SoftBreak | Event::HardBreak if self.skipping => self.skipping = false,
            // Single line breaks stay line breaks, as in Obsidian (unless strict line breaks join
            // them, as GitHub does). Each line is its own paragraph in the view, so Persian and
            // English lines each get their direction.
            Event::SoftBreak if self.joined => self.text(" "),
            Event::SoftBreak | Event::HardBreak => self.text("\n"),
            Event::Rule => {
                self.end_line();
                let mut end = self.buffer.end_iter();
                self.buffer.insert_with_tags(&mut end, " \n", &[&self.styles.rule]);
            }
            Event::TaskListMarker(done) => self.task(done, range),
        }
    }

    fn start(&mut self, tag: Tag, range: Range<usize>) {
        match tag {
            Tag::Paragraph => {
                if !self.pending_bullet {
                    self.end_line();
                }
            }
            Tag::Heading { level, .. } => {
                self.end_line();
                self.heading = Some((self.buffer.end_iter().offset(), String::new()));
                self.block.push(self.styles.headings[heading_index(level)].clone());
            }
            Tag::BlockQuote(kind) => {
                self.end_line();
                self.quote_depth += 1;
                // A callout: one of GitHub's five kinds (the parser has taken its marker), or
                // Obsidian's, from the quote's first line (drawn as the header instead).
                let first_line = self.source.get(range.start..).and_then(|rest| rest.lines().next()).unwrap_or("");
                let written = first_line.trim_start_matches(['>', ' ', '\t']);
                let callout = match kind {
                    Some(kind) => {
                        let (kind, title) = match kind {
                            BlockQuoteKind::Note => ("note", "Note"),
                            BlockQuoteKind::Tip => ("tip", "Tip"),
                            BlockQuoteKind::Important => ("tip", "Important"),
                            BlockQuoteKind::Warning => ("warning", "Warning"),
                            BlockQuoteKind::Caution => ("warning", "Caution"),
                        };
                        Some(obsidian::Callout { kind, title: title.to_owned(), fold: None })
                    }
                    None => obsidian::callout(written).inspect(|_| self.skipping = true),
                };
                let Some(callout) = callout else {
                    // A quote: a bar down its side, its text dimmer.
                    self.open_panel(Look::Bar(rgba(theme::colors().accent, 0.7)), 16);
                    self.block.push(self.styles.quote.clone());
                    return;
                };
                let color = callout_color(callout.kind);
                self.open_panel(Look::Box(rgba(color, 0.10)), 14);
                // Room above, so a callout right after another doesn't touch it.
                let header = gtk::TextTag::builder().weight(700).foreground_rgba(&rgba(color, 1.0)).pixels_above_lines(8).build();
                self.buffer.tag_table().add(&header);
                // Foldable: the header shows or hides the body ("+" open, "-" closed).
                let fold = callout.fold.map(|open| {
                    let fold = gtk::TextTag::builder().invisible(!open).build();
                    self.buffer.tag_table().add(&fold);
                    self.targets.push((header.clone(), Target::Fold(fold.clone())));
                    fold
                });
                // The title's own Markdown (bold, code, links…) drawn too.
                let mut title = inline_markup(&callout.title, self.find, self.text);
                match callout.fold {
                    Some(true) => title.push_str("  \u{2212}"),
                    Some(false) => title.push_str("  +"),
                    None => {}
                }
                self.markup(&title, &header);
                self.end_line();
                if let Some(fold) = fold {
                    let end = self.buffer.end_iter();
                    let body = self.buffer.create_mark(None, &end, true);
                    if let Some(open) = self.open.last_mut() {
                        self.panels[open.panel].fold = Some((fold.clone(), body));
                        open.fold = Some((end.offset(), fold));
                    }
                }
            }
            // Front matter shows as Obsidian's properties: a name, then its values.
            Tag::MetadataBlock(_) => {
                self.end_line();
                self.properties = Some(String::new());
            }
            Tag::CodeBlock(_) => {
                self.end_line();
                if let Tag::CodeBlock(CodeBlockKind::Fenced(info)) = &tag {
                    self.code_language = info.split_whitespace().next().map(str::to_owned);
                }
                // A diagram is drawn once all of its source is in.
                if self.code_language.as_deref() == Some("mermaid") {
                    self.mermaid = Some(String::new());
                    return;
                }
                self.code_start = Some(self.buffer.end_iter().offset());
                self.code_text = Some(String::new());
                self.block.push(self.styles.code_block.clone());
            }
            Tag::HtmlBlock => self.end_line(),
            Tag::List(first) => {
                if self.pending_bullet {
                    self.bullet();
                }
                // A checked task's strike-through ends with its own text, before its sub-items.
                if self.inline.last() == Some(&self.styles.done) {
                    self.inline.pop();
                }
                self.lists.push(List { next: first });
            }
            Tag::Item => {
                self.end_line();
                let indent = self.indent(self.left, self.list_depth(), 0, false);
                self.items.push(self.block.len());
                self.block.push(indent);
                self.pending_bullet = true;
            }
            Tag::FootnoteDefinition(label) => {
                self.end_line();
                self.styled(&format!("[{label}] "), self.styles.dim.clone(), "", "");
            }
            Tag::Table(aligns) => {
                self.end_line();
                self.table = Some(Table { aligns, rows: Vec::new(), head: false });
            }
            Tag::TableHead => self.with_table(|t| {
                t.head = true;
                t.rows.push((true, Vec::new()));
            }),
            Tag::TableRow => self.with_table(|t| t.rows.push((false, Vec::new()))),
            Tag::TableCell => self.with_table(|t| {
                if let Some((_, cells)) = t.rows.last_mut() {
                    cells.push(Cell::default());
                }
            }),
            Tag::Emphasis => self.push_inline(self.styles.emphasis.clone(), "<i>", "</i>"),
            Tag::Strong => self.push_inline(self.styles.strong.clone(), "<b>", "</b>"),
            Tag::Strikethrough => self.push_inline(self.styles.strike.clone(), "<s>", "</s>"),
            Tag::Link { link_type, dest_url, .. } => {
                let url = match link_type {
                    LinkType::Email => format!("mailto:{dest_url}"),
                    _ => dest_url.to_string(),
                };
                // A link to a note that doesn't exist yet is fainter, as in Obsidian.
                let path = links::split(&url).path;
                let missing = !links::has_scheme(&url) && !path.is_empty() && (self.find)(&path).is_none();
                let link = theme::colors().link;
                let tag = gtk::TextTag::builder()
                    .foreground_rgba(&gdk::RGBA::new(link.r as f32, link.g as f32, link.b as f32, if missing { 0.55 } else { 1.0 }))
                    .underline(pango::Underline::Single)
                    .build();
                self.buffer.tag_table().add(&tag);
                self.targets.push((tag.clone(), Target::Link(url)));
                self.push_inline(tag, "<u>", "</u>");
                self.text("\u{2068}");
            }
            Tag::Image { dest_url, .. } => self.image = Some((dest_url.to_string(), String::new())),
            // Not enabled in the parser options.
            Tag::Superscript | Tag::Subscript | Tag::DefinitionList | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.skipping = false;
                if self.lists.is_empty() && self.table.is_none() {
                    self.space_last_line(&self.styles.paragraph.clone());
                }
                self.end_line();
            }
            TagEnd::Heading(_) => {
                self.end_line();
                self.block.pop();
                if let Some((start, text)) = self.heading.take() {
                    let mark = self.buffer.create_mark(None, &self.buffer.iter_at_offset(start), true);
                    self.headings.push((text, mark));
                }
            }
            TagEnd::BlockQuote(_) => {
                self.skipping = false;
                self.end_line();
                self.quote_depth -= 1;
                if let Some(Open { fold: Some((start, fold)), .. }) = self.close_panel() {
                    self.buffer.apply_tag(&fold, &self.buffer.iter_at_offset(start), &self.buffer.end_iter());
                }
                if self.quote_depth == 0 {
                    self.gap();
                }
            }
            TagEnd::MetadataBlock(_) => {
                if let Some(yaml) = self.properties.take() {
                    self.show_properties(&yaml);
                }
            }
            TagEnd::CodeBlock => {
                // Flowcharts and sequence diagrams are drawn; other diagrams are shown as their code.
                if let Some(source) = self.mermaid.take() {
                    if let Some(chart) = flowchart::parse(&source) {
                        self.code_language = None;
                        return self.figure(flowchart::picture(&chart, self.text).upcast());
                    }
                    if let Some(chart) = state::parse(&source) {
                        self.code_language = None;
                        return self.figure(flowchart::picture(&chart, self.text).upcast());
                    }
                    if let Some(diagram) = classes::parse(&source) {
                        self.code_language = None;
                        return self.figure(classes::picture(&diagram, self.text).upcast());
                    }
                    if let Some(chart) = gantt::parse(&source) {
                        self.code_language = None;
                        return self.figure(gantt::picture(&chart, self.text).upcast());
                    }
                    if let Some(diagram) = sequence::parse(&source) {
                        self.code_language = None;
                        return self.figure(sequence::picture(&diagram, self.text).upcast());
                    }
                    self.code_start = Some(self.buffer.end_iter().offset());
                    self.code_text = Some(String::new());
                    self.block.push(self.styles.code_block.clone());
                    self.text(&source);
                }
                self.end_line();
                self.block.pop();
                if let Some(start) = self.code_start.take() {
                    // A click on the block copies its code (the fences left out), as the note has
                    // it: not what is shown (a list item's bullet, written into its first line).
                    let code = self.code_text.take().unwrap_or_default();
                    let (from, to) = (self.buffer.iter_at_offset(start), self.buffer.end_iter());
                    let copy = gtk::TextTag::new(None);
                    self.buffer.tag_table().add(&copy);
                    self.buffer.apply_tag(&copy, &from, &to);
                    self.targets.push((copy, Target::Copy(code.strip_suffix('\n').unwrap_or(&code).into())));
                    let first = self.buffer.create_mark(None, &self.buffer.iter_at_offset(start), true);
                    let last = self.buffer.create_mark(None, &self.buffer.end_iter(), true);
                    let look = Look::Box(rgba(theme::colors().code_background, 1.0));
                    // Its text is 12 px in from the margin; its box, 4.
                    self.panels.push(Panel { start: first, end: last, inset: self.margin + 4 - self.origin, look, rtl: false, fold: None });
                    if let Some(language) = self.code_language.take() {
                        let start = self.buffer.create_mark(None, &self.buffer.iter_at_offset(start), true);
                        let end = self.buffer.create_mark(None, &self.buffer.end_iter(), true);
                        self.code.push(CodeBlock { start, end, language });
                    }
                    self.space_code_block(start);
                }
                if self.quote_depth == 0 {
                    self.gap();
                }
            }
            TagEnd::HtmlBlock | TagEnd::FootnoteDefinition => self.end_line(),
            TagEnd::List(_) => {
                self.lists.pop();
            }
            TagEnd::Item => {
                if self.pending_bullet {
                    self.bullet();
                }
                self.end_line();
                self.block.pop();
                self.items.pop();
                // A checked task's strike-through ends with its item.
                if self.inline.last() == Some(&self.styles.done) {
                    self.inline.pop();
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    if let Some(tables) = &mut self.tables {
                        return tables.push(table);
                    }
                    let grid = table_widget(table);
                    let anchor = self.anchor();
                    self.children.push((anchor, grid));
                    self.end_line();
                }
            }
            TagEnd::TableHead | TagEnd::TableRow | TagEnd::TableCell => {}
            TagEnd::Link => {
                self.text("\u{2069}");
                self.pop_inline();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_inline(),
            TagEnd::Image => {
                if let Some((url, alt)) = self.image.take() {
                    // `![[Note]]` puts the note here; anything else is drawn as a picture.
                    if !self.embed(&url) {
                        self.image(&url, &alt);
                    }
                }
            }
            TagEnd::Superscript | TagEnd::Subscript | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle | TagEnd::DefinitionListDefinition => {}
        }
    }

    /// Plain text in the current styles.
    /// Text from the note at `range`: Obsidian's highlights and tags in it drawn as such (the
    /// `==` around a highlight left out), a task with its own mark (`[?]`, `[-]`) as done.
    fn text_at(&mut self, text: &str, range: Range<usize>) {
        if let Some(yaml) = &mut self.properties {
            return yaml.push_str(text);
        }
        let source = self.source;
        let written = source.get(range.clone()) == Some(text);
        // Already drawn (a task's own mark): left out.
        if written && range.end <= self.drawn_to {
            return;
        }
        let range = if written { range.start.max(self.drawn_to)..range.end } else { range };
        let text = if written { &source[range.clone()] } else { text };
        // A task with its own mark (`[?]`, `[-]`): the parser leaves it as text, often split at
        // the brackets, so it's read from the note.
        if self.pending_bullet
            && self.code_start.is_none()
            && let Some(&[b'[', mark, b']', b' ']) = source.as_bytes().get(range.start..range.start + 4)
            && !matches!(mark, b' ' | b'x' | b'X')
            && mark.is_ascii_graphic()
        {
            self.task(true, range.start..range.start + 3);
            self.drawn_to = range.start + 4;
            return self.text_at(text, range);
        }
        enum Mark {
            Highlight,
            Tag,
            Gone,
        }
        let overlapping = |list: &Vec<Range<usize>>| list.iter().filter(|r| r.start < range.end && range.start < r.end).cloned().collect::<Vec<_>>();
        let mut marks: Vec<(Range<usize>, Mark)> = overlapping(&self.highlights).into_iter().map(|r| (r, Mark::Highlight)).collect();
        marks.extend(self.tags.iter().filter(|t| t.start >= range.start && t.end <= range.end).map(|t| (t.clone(), Mark::Tag)));
        marks.extend(overlapping(&self.gone).into_iter().map(|r| (r, Mark::Gone)));
        if !written || marks.is_empty() || self.mermaid.is_some() || self.image.is_some() || self.code_start.is_some() {
            return self.text(text);
        }
        marks.sort_by_key(|(mark, _)| mark.start);
        let mut at = range.start;
        for (mark, kind) in marks {
            let start = mark.start.max(range.start).max(at);
            if start > at {
                self.text(&source[at..start]);
            }
            let end = mark.end.min(range.end).max(start);
            match kind {
                Mark::Tag => self.tag(&source[start..end]),
                Mark::Highlight => {
                    let (inner_start, inner_end) = ((mark.start + 2).max(start), (mark.end - 2).min(range.end));
                    if inner_start < inner_end {
                        let open = format!("<span background=\"{}\" bgalpha=\"32%\">", theme::colors().changed.to_hex());
                        self.styled(&source[inner_start..inner_end], self.styles.highlight.clone(), &open, "</span>");
                    }
                }
                // Left out, and a space after it too when one comes before it.
                Mark::Gone => {
                    if start > range.start && source.as_bytes()[start - 1] == b' ' && source.as_bytes().get(end) == Some(&b' ') {
                        at = end + 1;
                        continue;
                    }
                }
            }
            at = end;
        }
        if at < range.end {
            self.text(&source[at..range.end]);
        }
    }

    /// The note to draw (comments and block ids blanked) and Obsidian's additions in it.
    fn read(&mut self, shown: &'m str, additions: obsidian::Additions) {
        self.source = shown;
        self.line_starts = std::iter::once(0).chain(shown.match_indices('\n').map(|(i, _)| i + 1)).collect();
        self.highlights = additions.highlights;
        self.tags = additions.tags;
        self.gone = additions.comments.into_iter().chain(additions.block_ids).collect();
    }

    /// `![[Note]]` or `![[Note#Heading]]`: the note, or that part of it, drawn here in a frame
    /// under its name. Two notes deep at most: a note that embeds itself stops there. False when
    /// `url` isn't a note (a picture, say).
    fn embed(&mut self, url: &str) -> bool {
        if links::has_scheme(url) || self.table.is_some() {
            return false;
        }
        let target = links::split(url);
        let path = links::decode(&target.path);
        let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
        let file = (self.find)(&path);
        let is_note = |name: &str| name.to_lowercase().ends_with(".md");
        match &file {
            Some(file) if !file.basename().is_some_and(|base| is_note(&base.to_string_lossy())) => return false,
            None if !is_note(&name) && name.contains('.') => return false,
            _ => {}
        }
        let name = name.strip_suffix(".md").unwrap_or(&name);
        let title = match &target.heading {
            Some(heading) => format!("{name} \u{203A} {heading}"),
            None => name.to_owned(),
        };
        // Its name links to it; fainter when it doesn't exist yet, as other links.
        let link = theme::colors().link;
        let alpha = if file.is_some() { 1.0 } else { 0.55 };
        let tag = gtk::TextTag::builder()
            .foreground_rgba(&gdk::RGBA::new(link.r as f32, link.g as f32, link.b as f32, alpha))
            .weight(600)
            .build();
        self.buffer.tag_table().add(&tag);
        self.targets.push((tag.clone(), Target::Link(url.to_owned())));
        let text = file
            .as_ref()
            .filter(|_| self.depth < 2)
            .and_then(|file| file.load_contents(None::<&gio::Cancellable>).ok())
            .map(|(bytes, _)| String::from_utf8_lossy(&bytes).into_owned());
        let (Some(file), Some(text)) = (file, text) else {
            // Missing, unreadable or too deep: just its name.
            self.styled(&title, tag, "", "");
            return true;
        };
        let part = match &target.heading {
            Some(heading) => section(&text, heading),
            None => text,
        };
        self.end_line();
        self.open_panel(Look::Bar(rgba(theme::colors().link, 0.9)), 16);
        self.styled(&title, tag, "", "");
        self.end_line();
        let (shown, additions) = prepare(&part);
        let mut nested = Renderer::new(file.parent().as_ref(), self.find, self.buffer.clone(), self.text, self.left);
        nested.depth = self.depth + 1;
        nested.origin = self.origin;
        nested.read(&shown, additions);
        nested.frame = self.frame.clone();
        for (event, range) in events(&shown) {
            nested.event(event, range);
        }
        nested.end_line();
        nested.mirror_rtl_indents();
        // Its checkboxes are the other note's: not ticked from here.
        self.targets.extend(nested.targets.into_iter().filter(|(_, target)| !matches!(target, Target::Task(_))));
        self.children.extend(nested.children);
        self.code.extend(nested.code);
        self.panels.extend(nested.panels);
        self.close_panel();
        self.gap();
        true
    }

    /// `markup` (Pango's) at the end, in `tag` and the tags in force: a callout's title.
    fn markup(&mut self, markup: &str, tag: &gtk::TextTag) {
        if self.pending_bullet {
            self.bullet();
        }
        // A text buffer knows neither of the attributes Pango's <sup> and <sub> make (it warns,
        // and leaves the script level): raised and smaller instead.
        let markup = markup
            .replace("<sup>", "<span rise=\"4000\" size=\"smaller\">")
            .replace("<sub>", "<span rise=\"-2000\" size=\"smaller\">")
            .replace("</sup>", "</span>")
            .replace("</sub>", "</span>");
        let start = self.buffer.end_iter().offset();
        let mut end = self.buffer.end_iter();
        self.buffer.insert_markup(&mut end, &markup);
        let start = self.buffer.iter_at_offset(start);
        for tag in self.frame.iter().chain(&self.block).chain(&self.inline).chain([tag]) {
            self.buffer.apply_tag(tag, &start, &end);
        }
    }

    /// Marks where the block starting at byte `at` of the note starts in the view, by its line
    /// (the note's own lines only, not an embedded note's).
    fn mark_line(&mut self, at: usize) {
        if self.depth > 0 {
            return;
        }
        let line = self.line_starts.partition_point(|&start| start <= at).saturating_sub(1);
        if self.lines.last().is_none_or(|(last, _)| *last < line) {
            let mark = self.buffer.create_mark(None, &self.buffer.end_iter(), true);
            self.lines.push((line, mark));
        }
    }

    /// `#tag`, as a small clickable label that searches for it.
    fn tag(&mut self, tag: &str) {
        let accent = theme::colors().accent;
        let pill = gtk::TextTag::builder().foreground(accent.to_hex()).build();
        self.buffer.tag_table().add(&pill);
        self.targets.push((pill.clone(), Target::Tag(tag.to_owned())));
        self.chip(tag, pill, "", "", rgba(accent, 0.14));
    }

    /// `text` as `styled` writes it, on a chip of `color` the view draws (out of a table: in
    /// one, a label's markup has no chips).
    fn chip(&mut self, text: &str, tag: gtk::TextTag, open: &str, close: &str, color: gdk::RGBA) {
        let start = self.buffer.end_iter().offset() as usize;
        self.styled(text, tag, open, close);
        let end = self.buffer.end_iter().offset() as usize;
        if self.table.is_none() && end > start {
            self.chips.push((start..end, color));
        }
    }

    /// Front matter as Obsidian's properties: each name, then its values (tags as tags, links as
    /// links).
    fn show_properties(&mut self, yaml: &str) {
        let rows = obsidian::properties(yaml);
        if rows.is_empty() {
            return;
        }
        let mut tabs = pango::TabArray::new(1, true);
        tabs.set_tab(0, pango::TabAlign::Left, 150);
        let colors = theme::colors();
        let row = gtk::TextTag::builder().tabs(&tabs).left_margin(self.margin).pixels_below_lines(4).build();
        self.buffer.tag_table().add(&row);
        self.block.push(row);
        for (name, values) in rows {
            self.styled(&format!("{name}\t"), self.styles.dim.clone(), "", "");
            let tags = name.eq_ignore_ascii_case("tags");
            for (i, value) in values.iter().enumerate() {
                if i > 0 {
                    self.text(if tags { " " } else { ", " });
                }
                if tags {
                    self.tag(&format!("#{}", value.trim_start_matches('#')));
                } else if let Some(target) = value.strip_prefix("[[").and_then(|v| v.strip_suffix("]]")) {
                    let words = target.split_once('|').map_or(target, |(_, words)| words);
                    let link = gtk::TextTag::builder().foreground(colors.link.to_hex()).underline(pango::Underline::Single).build();
                    self.buffer.tag_table().add(&link);
                    self.targets.push((link.clone(), Target::Link(target.split('|').next().unwrap_or(target).to_owned())));
                    self.styled(words, link, "", "");
                } else {
                    self.text(value);
                }
            }
            self.text("\n");
        }
        self.block.pop();
        self.gap();
    }

    fn text(&mut self, text: &str) {
        if let Some(source) = &mut self.mermaid {
            source.push_str(text);
            return;
        }
        if let Some((_, alt)) = &mut self.image {
            alt.push_str(text);
            return;
        }
        if let Some(code) = &mut self.code_text {
            code.push_str(text);
        }
        if let Some(cell) = self.cell() {
            cell.markup.push_str(&glib::markup_escape_text(text));
            cell.plain.push_str(text);
            return;
        }
        if self.pending_bullet {
            self.bullet();
        }
        if let Some((_, heading)) = &mut self.heading {
            heading.push_str(text);
        }
        let tags: Vec<&gtk::TextTag> = self.frame.iter().chain(&self.block).chain(&self.inline).collect();
        let mut end = self.buffer.end_iter();
        self.buffer.insert_with_tags(&mut end, text, &tags);
    }

    /// Text with one extra style (or Pango markup inside a table cell).
    fn styled(&mut self, text: &str, tag: gtk::TextTag, open: &str, close: &str) {
        if let Some(cell) = self.cell() {
            cell.markup.push_str(open);
            cell.markup.push_str(&glib::markup_escape_text(text));
            cell.markup.push_str(close);
            cell.plain.push_str(text);
            return;
        }
        self.inline.push(tag);
        self.text(text);
        self.inline.pop();
    }

    fn push_inline(&mut self, tag: gtk::TextTag, open: &'static str, close: &'static str) {
        if let Some(cell) = self.cell() {
            cell.markup.push_str(open);
            self.markup_close.push(close);
        } else {
            self.inline.push(tag);
        }
    }

    fn pop_inline(&mut self) {
        if self.table.is_some() {
            if let Some(close) = self.markup_close.pop()
                && let Some(cell) = self.cell()
            {
                cell.markup.push_str(close);
            }
        } else {
            self.inline.pop();
        }
    }

    fn cell(&mut self) -> Option<&mut Cell> {
        self.table.as_mut()?.rows.last_mut()?.1.last_mut()
    }

    fn with_table(&mut self, f: impl FnOnce(&mut Table)) {
        if let Some(table) = &mut self.table {
            f(table);
        }
    }

    /// A formula, its scripts raised (or lowered) and smaller; a display formula on centred
    /// lines of its own.
    fn math(&mut self, latex: &str, display: bool) {
        let runs = math::runs(latex);
        if let Some(cell) = self.cell() {
            cell.markup.push_str(&math_markup(latex, true));
            cell.plain.extend(runs.iter().map(|run| run.text.as_str()));
            return;
        }
        if display {
            self.end_line();
            self.block.push(self.styles.display_math.clone());
        } else {
            // Isolated like code, so a formula inside Persian keeps its order.
            self.text("\u{2068}");
        }
        self.inline.push(self.styles.math.clone());
        for run in &runs {
            let script = self.script(run.rise, run.depth);
            self.inline.extend(script.clone());
            self.text(&if display { run.text.clone() } else { run.text.replace('\n', "; ") });
            if script.is_some() {
                self.inline.pop();
            }
        }
        self.inline.pop();
        if display {
            self.end_line();
            self.block.pop();
        } else {
            self.text("\u{2069}");
        }
    }

    /// The tag for a formula's script `rise` fifths of an em up (down when negative) and
    /// `depth` scripts deep; none on the baseline.
    fn script(&mut self, rise: i8, depth: u8) -> Option<gtk::TextTag> {
        if rise == 0 && depth == 0 {
            return None;
        }
        let em = self.measure("M").max(8);
        let table = self.buffer.tag_table();
        let tag = self.scripts.entry((rise, depth)).or_insert_with(|| {
            let tag = gtk::TextTag::builder()
                .rise(i32::from(rise) * em * pango::SCALE / 5)
                .scale(if depth <= 1 { 0.72 } else { 0.58 })
                .build();
            table.add(&tag);
            tag
        });
        Some(tag.clone())
    }

    /// Writes the pending list bullet ("•", "◦", "▪" or "3.").
    fn bullet(&mut self) {
        self.pending_bullet = false;
        let depth = self.lists.len();
        let marker = match self.lists.last_mut().and_then(|l| l.next.as_mut()) {
            Some(n) => {
                *n += 1;
                format!("{}. ", *n - 1)
            }
            None => match depth {
                0 | 1 => "• ".to_owned(),
                2 => "◦ ".to_owned(),
                _ => "▪ ".to_owned(),
            },
        };
        self.hang(&marker);
        self.inline.push(self.styles.dim.clone());
        // Shown, not part of a code block it may start.
        let code = self.code_text.take();
        self.text(&marker);
        self.code_text = code;
        self.inline.pop();
    }

    /// Indents the open list item so its wrapped lines start under its text, not its marker.
    fn hang(&mut self, marker: &str) {
        let Some(&slot) = self.items.last() else { return };
        let indent = self.indent(self.left, self.list_depth(), self.measure(marker), false);
        self.block[slot] = indent;
    }

    fn task(&mut self, done: bool, range: Range<usize>) {
        self.pending_bullet = false;
        self.hang(if done { "☑ " } else { "☐ " });
        let colors = theme::colors();
        // Out of a table, the glyph only makes room for the box the view draws over it.
        let drawn = self.table.is_none();
        let tag = if drawn {
            gtk::TextTag::builder().foreground_rgba(&gdk::RGBA::new(0.0, 0.0, 0.0, 0.001)).build()
        } else {
            gtk::TextTag::builder().foreground((if done { colors.accent } else { colors.dim }).to_hex()).build()
        };
        self.buffer.tag_table().add(&tag);
        self.targets.push((tag.clone(), Target::Task(range)));
        let at = self.buffer.end_iter().offset() as usize;
        self.styled(if done { "☑ " } else { "☐ " }, tag, "", "");
        if drawn {
            self.marks.push((at..at + 1, crate::style::Mark::Task(done)));
        }
        if done {
            self.inline.push(self.styles.done.clone());
        }
    }

    fn image(&mut self, url: &str, alt: &str) {
        if self.table.is_some() {
            return self.text(&format!("[image: {alt}]"));
        }
        let local = !url.contains("://");
        // "Pasted%20image.png" is the file "Pasted image.png", as in links; found beside the
        // note, or anywhere in the vault by its name, as Obsidian finds embeds.
        let path = links::decode(url);
        let file = || {
            (self.find)(&path)
                .or_else(|| self.folder.as_ref().map(|f| f.resolve_relative_path(&path)))
                .unwrap_or_else(|| gio::File::for_path(&path))
        };
        let texture = local.then(file).and_then(|file| gdk::Texture::from_file(&file).ok());
        let (alt, _) = caption_and_size(alt);
        let Some(texture) = texture else {
            // Remote or unreadable: show the alt text as a link instead.
            let label = if alt.is_empty() { url } else { alt };
            let tag = gtk::TextTag::builder().foreground(theme::colors().link.to_hex()).underline(pango::Underline::Single).build();
            self.buffer.tag_table().add(&tag);
            self.targets.push((tag.clone(), Target::Link(url.to_owned())));
            self.styled(&format!("[image: {label}]"), tag, "", "");
            return;
        };
        let (width, height) = picture_box(&texture, caption_and_size(alt).1);
        let picture = gtk::Picture::for_paintable(&texture);
        picture.set_can_shrink(true);
        picture.set_size_request(width, height);
        if !alt.is_empty() {
            picture.set_alternative_text(Some(alt));
        }
        let anchor = self.anchor();
        self.children.push((anchor, picture.upcast()));
    }

    /// A diagram on a centred line of its own.
    fn figure(&mut self, widget: gtk::Widget) {
        self.end_line();
        let anchor = self.anchor();
        let end = self.buffer.end_iter();
        let mut start = end;
        start.backward_char();
        self.buffer.apply_tag(&self.styles.figure, &start, &end);
        self.children.push((anchor, widget));
        self.end_line();
        if self.quote_depth == 0 {
            self.gap();
        }
    }

    /// A place for a picture, a table or a diagram at the end, in the panel's margins if on one.
    fn anchor(&mut self) -> gtk::TextChildAnchor {
        let mut end = self.buffer.end_iter();
        let anchor = self.buffer.create_child_anchor(&mut end);
        if let Some(frame) = &self.frame {
            let mut start = end;
            start.backward_char();
            self.buffer.apply_tag(frame, &start, &end);
        }
        anchor
    }

    /// Starts a block on a panel (see `Panel`): a quote, a callout, an embedded note. The panel's
    /// edge is where text starts now (in a list item: at the item's text), and the block's text
    /// starts `pad` pixels in from it, on both sides.
    fn open_panel(&mut self, look: Look, pad: i32) {
        let edge = match self.items.last() {
            Some(&slot) => self.block[slot].left_margin(),
            None => self.left,
        };
        let start = self.buffer.create_mark(None, &self.buffer.end_iter(), true);
        self.panels.push(Panel { start: start.clone(), end: start, inset: edge - self.origin, look, rtl: false, fold: None });
        self.open.push(Open {
            panel: self.panels.len() - 1,
            blocks: self.block.len(),
            left: self.left,
            list_base: self.list_base,
            frame: self.frame.take(),
            fold: None,
        });
        self.left = edge + pad;
        self.list_base = self.lists.len();
        let table = self.buffer.tag_table();
        let left = self.left;
        let frame = self.frames.entry(left).or_insert_with(|| {
            let tag = gtk::TextTag::builder().left_margin(left).right_margin(left).pixels_above_lines(2).pixels_below_lines(2).build();
            table.add(&tag);
            tag.set_priority(0);
            tag
        });
        self.frame = Some(frame.clone());
    }

    /// Ends the innermost panel's block, and says how it was opened.
    fn close_panel(&mut self) -> Option<Open> {
        let open = self.open.pop()?;
        self.block.truncate(open.blocks);
        self.left = open.left;
        self.list_base = open.list_base;
        self.frame = open.frame.clone();
        let end = self.buffer.end_iter();
        let panel = &mut self.panels[open.panel];
        panel.end = self.buffer.create_mark(None, &end, true);
        let start = self.buffer.iter_at_mark(&panel.start);
        panel.rtl = pango::find_base_dir(&self.buffer.text(&start, &end, true)) == pango::Direction::Rtl;
        Some(open)
    }

    /// How deep the list being written is, counted inside the innermost panel.
    fn list_depth(&self) -> usize {
        self.lists.len().saturating_sub(self.list_base)
    }

    /// Ends the current line unless the buffer is empty or already at a line start.
    fn end_line(&mut self) {
        let mut end = self.buffer.end_iter();
        if end.offset() > 0 && end.backward_char() && end.char() != '\n' {
            let mut end = self.buffer.end_iter();
            self.buffer.insert(&mut end, "\n");
        }
    }

    /// A list item's indent: its depth's margin, with the first line pulled back by `hang`
    /// pixels so the marker sits in the margin and wrapped lines line up with the text.
    fn indent(&mut self, left: i32, depth: usize, hang: i32, rtl: bool) -> gtk::TextTag {
        let table = self.buffer.tag_table();
        self.indents
            .entry((left, depth, hang, rtl))
            .or_insert_with(|| {
                let margin = left + 6 + 22 * (depth.max(1) as i32 - 1) + hang;
                let tag = if rtl {
                    gtk::TextTag::builder().right_margin(margin).indent(-hang).pixels_below_lines(4).build()
                } else {
                    gtk::TextTag::builder().left_margin(margin).indent(-hang).pixels_below_lines(4).build()
                };
                table.add(&tag);
                tag
            })
            .clone()
    }

    /// An empty, short line: separates blocks whose background covers their own spacing.
    fn gap(&mut self) {
        self.end_line();
        let mut end = self.buffer.end_iter();
        self.buffer.insert_with_tags(&mut end, "\n", &[&self.styles.gap]);
    }

    /// Applies `tag` (spacing) to the line being written, from its start to the end.
    fn space_last_line(&self, tag: &gtk::TextTag) {
        let end = self.buffer.end_iter();
        let mut start = end;
        start.set_line_offset(0);
        self.buffer.apply_tag(tag, &start, &end);
    }

    /// Extra space above the first and below the last line of a code block.
    fn space_code_block(&self, start: i32) {
        let first = self.buffer.iter_at_offset(start);
        let mut first_end = first;
        first_end.forward_to_line_end();
        self.buffer.apply_tag(&self.styles.code_top, &first, &first_end);
        let mut last_end = self.buffer.end_iter();
        last_end.backward_char();
        let mut last = last_end;
        last.set_line_offset(0);
        self.buffer.apply_tag(&self.styles.code_bottom, &last, &last_end);
    }

    fn finish(mut self) -> Rendered {
        self.mirror_rtl_indents();
        // Drop the trailing newline so the view doesn't end with an empty line.
        let mut end = self.buffer.end_iter();
        let mut last = end;
        if last.backward_char() && last.char() == '\n' {
            self.buffer.delete(&mut last, &mut end);
        }
        self.buffer.set_modified(false);
        let start = self.buffer.start_iter();
        self.buffer.place_cursor(&start);
        Rendered {
            buffer: self.buffer,
            targets: self.targets,
            children: self.children,
            headings: self.headings,
            lines: self.lines,
            code: self.code,
            panels: self.panels,
            marks: self.marks,
            chips: self.chips,
        }
    }

    /// List indents are on the left; move them to the right on right-to-left lines.
    fn mirror_rtl_indents(&mut self) {
        let ltr: Vec<((i32, usize, i32), gtk::TextTag)> = self
            .indents
            .iter()
            .filter(|((_, _, _, rtl), _)| !rtl)
            .map(|((left, depth, hang, _), tag)| ((*left, *depth, *hang), tag.clone()))
            .collect();
        for line in 0..self.buffer.line_count() {
            let Some(start) = self.buffer.iter_at_line(line) else { continue };
            let Some(((left, depth, hang), tag)) = ltr.iter().find(|(_, tag)| start.has_tag(tag)) else { continue };
            let mut end = start;
            end.forward_to_line_end();
            if pango::find_base_dir(&self.buffer.text(&start, &end, true)) == pango::Direction::Rtl {
                let rtl = self.indent(*left, *depth, *hang, true);
                self.buffer.remove_tag(tag, &start, &end);
                self.buffer.apply_tag(&rtl, &start, &end);
            }
        }
    }
}

/// Colours fenced code blocks with GtkSourceView's highlighting for their language. Meant to
/// run after the first frame: loading the language definitions takes ~100 ms.
pub fn highlight_code(buffer: &sourceview::Buffer, blocks: &[CodeBlock], scheme: Option<&sourceview::StyleScheme>) {
    let table = buffer.tag_table();
    let mut colours: HashMap<String, gtk::TextTag> = HashMap::new();
    for block in blocks {
        let start = buffer.iter_at_mark(&block.start);
        let code = buffer.text(&start, &buffer.iter_at_mark(&block.end), true);
        let base = start.offset() as usize;
        for (range, colour) in code_colours(&block.language, &code, scheme) {
            let tag = colours.entry(colour.to_string()).or_insert_with(|| {
                let tag = gtk::TextTag::builder().foreground_rgba(&colour).build();
                table.add(&tag);
                tag
            });
            let at = |offset: usize| buffer.iter_at_offset((base + offset) as i32);
            buffer.apply_tag(tag, &at(range.start), &at(range.end));
        }
    }
}

/// How GtkSourceView colours `code` as `language` (a fence's name: "rust", "py", "sh"…):
/// character ranges and their colours. Nothing for a language it doesn't know.
pub fn code_colours(language: &str, code: &str, scheme: Option<&sourceview::StyleScheme>) -> Vec<(Range<usize>, gdk::RGBA)> {
    let manager = sourceview::LanguageManager::default();
    let Some(language) = self::language(&manager, language) else { return Vec::new() };
    // Highlighted in a scratch buffer, its colours read back.
    let scratch = sourceview::Buffer::new(None);
    scratch.set_style_scheme(scheme);
    scratch.set_language(Some(&language));
    scratch.set_text(code);
    scratch.ensure_highlight(&scratch.start_iter(), &scratch.end_iter());
    let mut found = Vec::new();
    let mut iter = scratch.start_iter();
    loop {
        let from = iter.offset() as usize;
        let colour = foreground(&iter);
        let more = iter.forward_to_tag_toggle(None::<&gtk::TextTag>);
        if let Some(colour) = colour {
            found.push((from..iter.offset() as usize, colour));
        }
        if !more {
            break;
        }
    }
    found
}

/// The colour the highlighting gives the text at `iter`, if any.
fn foreground(iter: &gtk::TextIter) -> Option<gdk::RGBA> {
    iter.tags().into_iter().rev().find(|tag| tag.is_foreground_set()).and_then(|tag| tag.foreground_rgba())
}

/// GtkSourceView's language for a fence's name: its id, a common alias, or a file extension.
fn language(manager: &sourceview::LanguageManager, name: &str) -> Option<sourceview::Language> {
    let name = name.to_ascii_lowercase();
    let id = match name.as_str() {
        "py" | "python" => "python3",
        "js" | "javascript" => "js",
        "ts" => "typescript",
        "sh" | "bash" | "shell" | "zsh" | "console" => "sh",
        "rs" => "rust",
        "yml" => "yaml",
        "c++" => "cpp",
        "md" => "markdown",
        other => other,
    };
    manager.language(id).or_else(|| manager.guess_language(Some(format!("file.{name}")), None))
}

/// A note as it is drawn: Obsidian's %%comments%% and ^block ids blanked with spaces (so
/// everything else stays where it is in the note, and a checkbox click finds its place), and
/// Obsidian's additions in it.
fn prepare(source: &str) -> (String, obsidian::Additions) {
    let additions = obsidian::additions(source);
    let mut shown = source.to_owned();
    for range in additions.comments.iter().chain(&additions.block_ids) {
        shown.replace_range(range.clone(), &" ".repeat(range.len()));
    }
    (shown, additions)
}

/// The part of `text` under `heading` (as a `#heading` link names it): from that heading to
/// the next one as big or bigger; all of it when there's no such heading.
fn section(text: &str, heading: &str) -> String {
    // `#^id`: the block the id names.
    if let Some(id) = heading.strip_prefix('^') {
        return obsidian::block(text, id).map_or_else(|| text.to_owned(), |block| text[block].to_owned());
    }
    let headings = crate::outline::headings(text);
    let Some(at) = headings.iter().position(|h| crate::anchors::points_at(heading, &h.text)) else { return text.to_owned() };
    let level = headings[at].level;
    let end = headings[at + 1..].iter().find(|h| h.level <= level).map(|h| h.line);
    let lines: Vec<&str> = text.lines().collect();
    lines[headings[at].line..end.unwrap_or(lines.len())].join("\n")
}

/// A theme colour as GDK's, `alpha` opaque.
pub fn rgba(color: crate::color::Rgb, alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(color.r as f32, color.g as f32, color.b as f32, alpha)
}

/// A callout kind's colour, from the theme: blue for notes, green for success, yellow for
/// questions, orange for warnings, red for failures and bugs…
pub fn callout_color(kind: &str) -> crate::color::Rgb {
    let c = theme::colors();
    match kind {
        "abstract" | "tip" => c.function,
        "success" => c.added,
        "question" => c.changed,
        "warning" => c.number,
        "failure" | "danger" | "bug" => c.removed,
        "example" => c.keyword,
        "quote" => c.dim,
        _ => c.link,
    }
}

/// Obsidian's sizes: `![[pic.png|300]]` (the text after `|` comes as the alt) and
/// `![caption|300x200](pic.png)`: the caption, and the size if one is given.
fn caption_and_size(alt: &str) -> (&str, Option<(i32, Option<i32>)>) {
    match alt.rsplit_once('|') {
        Some((caption, size)) if picture_size(size).is_some() => (caption, picture_size(size)),
        _ => match picture_size(alt) {
            Some(size) => ("", Some(size)),
            None => (alt, None),
        },
    }
}

/// How big a picture is shown: at most 720 px wide unless a size is given, keeping the aspect
/// ratio unless both sides are.
fn picture_box(texture: &gdk::Texture, size: Option<(i32, Option<i32>)>) -> (i32, i32) {
    let (w, h) = (texture.width().max(1), texture.height().max(1));
    match size {
        Some((width, Some(height))) => (width, height),
        Some((width, None)) => (width, h * width / w),
        None => (w.min(720), h * w.min(720) / w),
    }
}

/// The picture `source` (`![…](…)` or `![[…]]`) shows, found as the rendered view finds it, and
/// how big: for Live Preview. Nothing for one on the web or not there.
pub fn picture(source: &str, find: &dyn Fn(&str) -> Option<gio::File>) -> Option<(gdk::Texture, i32, i32)> {
    let mut url = None;
    let mut alt = String::new();
    for (event, _) in events(source) {
        match event {
            Event::Start(Tag::Image { dest_url, .. }) => url = Some(dest_url.to_string()),
            Event::Text(text) if url.is_some() => alt.push_str(&text),
            Event::End(TagEnd::Image) => break,
            _ => {}
        }
    }
    let url = url.filter(|url| !links::has_scheme(url))?;
    let texture = gdk::Texture::from_file(&find(&links::decode(&url))?).ok()?;
    let (width, height) = picture_box(&texture, caption_and_size(&alt).1);
    Some((texture, width, height))
}

/// The note `source` (`![[Note]]`, `![[Note#Heading]]`, `![](Note.md)`) embeds, found as the
/// rendered view finds it: its file, its title (its name, and the heading) and its text (that
/// part of it). For Live Preview; nothing when it isn't a note there.
pub fn embedded(source: &str, find: &dyn Fn(&str) -> Option<gio::File>) -> Option<(gio::File, String, String)> {
    let url = events(source).find_map(|(event, _)| match event {
        Event::Start(Tag::Image { dest_url, .. }) => Some(dest_url.to_string()),
        _ => None,
    })?;
    let target = links::split(&url);
    let path = links::decode(&target.path);
    let note = |file: &gio::File| file.basename().is_some_and(|name| name.to_string_lossy().to_lowercase().ends_with(".md"));
    let file = find(&path).filter(note)?;
    let (bytes, _) = file.load_contents(None::<&gio::Cancellable>).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let name = path.rsplit('/').next().unwrap_or(&path);
    let name = name.strip_suffix(".md").unwrap_or(name).to_owned();
    Some(match &target.heading {
        Some(heading) => (file, format!("{name} \u{203A} {heading}"), section(&text, heading)),
        None => (file, name, text),
    })
}

/// The table `source` is, its cells as the rendered view writes them: for Live Preview.
pub fn table(source: &str, find: &dyn Fn(&str) -> Option<gio::File>, text: &dyn Fn(&str) -> pango::Layout) -> Option<Table> {
    let mut r = Renderer::new(None, find, sourceview::Buffer::new(None), text, 0);
    r.tables = Some(Vec::new());
    let (shown, additions) = prepare(source);
    r.read(&shown, additions);
    for (event, range) in events(&shown) {
        r.event(event, range);
    }
    r.tables?.into_iter().next()
}

/// `source`'s inline Markdown (bold, italic, code, links, formulas…) as Pango markup, as table
/// cells are written: a callout's title.
pub fn inline_markup(source: &str, find: &dyn Fn(&str) -> Option<gio::File>, text: &dyn Fn(&str) -> pango::Layout) -> String {
    let mut r = Renderer::new(None, find, sourceview::Buffer::new(None), text, 0);
    r.table = Some(Table { aligns: Vec::new(), rows: vec![(false, vec![Cell::default()])], head: false });
    let (shown, additions) = prepare(source);
    r.read(&shown, additions);
    for (event, range) in events(&shown) {
        if !matches!(event, Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph)) {
            r.event(event, range);
        }
    }
    r.cell().map(|cell| std::mem::take(&mut cell.markup)).unwrap_or_default()
}

/// A formula (what is between the dollar signs) as Pango markup: its runs raised or lowered,
/// smaller in scripts; on `one_line`, its lines joined by semicolons.
pub fn math_markup(latex: &str, one_line: bool) -> String {
    let mut markup = format!("<span font_family=\"{}\">", fonts::MATH);
    for run in math::runs(latex) {
        let script = match run.rise {
            rise if rise > 0 => "sup",
            rise if rise < 0 => "sub",
            _ => "small",
        };
        let depth = usize::from(run.depth);
        let text = if one_line { run.text.replace('\n', "; ") } else { run.text };
        let text = glib::markup_escape_text(&text);
        markup.push_str(&format!("{}{text}{}", format!("<{script}>").repeat(depth), format!("</{script}>").repeat(depth)));
    }
    markup.push_str("</span>");
    markup
}

/// An Obsidian picture size: "300" (the width) or "300x200".
fn picture_size(text: &str) -> Option<(i32, Option<i32>)> {
    let number = |text: &str| text.trim().parse::<i32>().ok().filter(|&n| n > 0 && n <= 4000);
    match text.split_once('x') {
        Some((width, height)) => Some((number(width)?, Some(number(height)?))),
        None => Some((number(text)?, None)),
    }
}

fn heading_index(level: HeadingLevel) -> usize {
    level as usize - 1
}

/// Width in columns: CJK and other full-width characters count twice.
pub fn display_width(text: &str) -> usize {
    text.chars()
        .map(|c| match c as u32 {
            0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F
            | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 => 2,
            _ => 1,
        })
        .sum()
}

fn table_widget(table: Table) -> gtk::Widget {
    // No spacing: each cell pads itself, so the rules under the rows run unbroken.
    let grid = gtk::Grid::builder().build();
    for (row, (header, cells)) in table.rows.iter().enumerate() {
        for (col, cell) in cells.iter().enumerate() {
            let rtl = pango::find_base_dir(&cell.plain) == pango::Direction::Rtl;
            let xalign = match table.aligns.get(col) {
                Some(Alignment::Center) => 0.5,
                Some(Alignment::Right) => 1.0,
                Some(Alignment::Left) => 0.0,
                // Unaligned columns start where the cell's own text starts.
                _ if rtl => 1.0,
                _ => 0.0,
            };
            let markup = if *header { format!("<span weight=\"600\">{}</span>", cell.markup) } else { cell.markup.clone() };
            // Embedded widgets get their minimum width, so only long cells may wrap.
            let long = display_width(&cell.plain) > 30;
            let label = gtk::Label::builder()
                .use_markup(true)
                .label(markup)
                .wrap(long)
                .width_chars(if long { 30 } else { -1 })
                .xalign(xalign)
                .css_classes([if *header { "nevis-table-head" } else { "nevis-table-cell" }])
                .build();
            if row + 1 == table.rows.len() {
                label.add_css_class("last");
            }
            grid.attach(&label, col as i32, row as i32, 1, 1);
        }
    }
    let frame = gtk::Frame::builder().child(&grid).margin_bottom(10).build();
    frame.add_css_class("nevis-table");
    frame.upcast()
}
