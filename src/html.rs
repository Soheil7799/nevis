//! A note as one HTML file: its Markdown as a page in the PDF's colours (and the window's,
//! for a reader in dark mode), its pictures inside it, so that it can be sent on its own.

use std::path::Path;

use gtk::{gio, glib, prelude::*};
use pulldown_cmark::{Event, LinkType, Parser, Tag, TagEnd};

use crate::palettes::Colors;
use crate::render::OPTIONS;

/// The page for `markdown`, called `title`; pictures are looked up with `find` (a note's
/// links, as the rendered view follows them), else beside the note in `folder`.
pub fn page(title: &str, markdown: &str, folder: Option<&Path>, find: &dyn Fn(&str) -> Option<gio::File>, light: &Colors, dark: &Colors) -> String {
    let mut in_metadata = false;
    let events = Parser::new_ext(markdown, OPTIONS).filter_map(|event| match event {
        // Front matter is for the notes app, not the reader.
        Event::Start(Tag::MetadataBlock(_)) => {
            in_metadata = true;
            None
        }
        Event::End(TagEnd::MetadataBlock(_)) => {
            in_metadata = false;
            None
        }
        _ if in_metadata => None,
        Event::Start(Tag::Image { link_type, dest_url, title, id }) => {
            let dest_url = picture(&dest_url, link_type, folder, find).map_or(dest_url, Into::into);
            Some(Event::Start(Tag::Image { link_type, dest_url, title, id }))
        }
        event => Some(event),
    });
    let mut body = String::new();
    pulldown_cmark::html::push_html(&mut body, events);
    format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{}</title>\n<style>\n{}</style>\n</head>\n<body>\n<main>\n{body}</main>\n</body>\n</html>\n",
        glib::markup_escape_text(title),
        style(light, dark)
    )
}

/// A local picture as a data: address, so the page carries it; `None` for one on the web or
/// one not found.
fn picture(dest: &str, link_type: LinkType, folder: Option<&Path>, find: &dyn Fn(&str) -> Option<gio::File>) -> Option<String> {
    if crate::links::has_scheme(dest) {
        return None;
    }
    let decoded = crate::links::decode(dest);
    let path = find(&decoded)
        .and_then(|file| file.path())
        .filter(|path| path.is_file())
        .or_else(|| (link_type != LinkType::WikiLink { has_pothole: false }).then(|| folder.map(|f| f.join(&decoded))).flatten())
        .filter(|path| path.is_file())?;
    let mime = match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        _ => return None,
    };
    let bytes = std::fs::read(&path).ok()?;
    Some(format!("data:{mime};base64,{}", glib::base64_encode(&bytes)))
}

fn style(light: &Colors, dark: &Colors) -> String {
    let colors = |c: &Colors| {
        let h = |color: crate::color::Rgb| color.to_hex();
        format!(
            "--bg: {}; --text: {}; --dim: {}; --link: {}; --heading: {}; --code: {}; --code-bg: {}; --quote: {}; --quote-bg: {}; --border: {}; --accent: {};",
            h(c.background),
            h(c.text),
            h(c.dim),
            h(c.link),
            h(c.heading),
            h(c.code),
            h(c.code_background),
            h(c.quote),
            h(c.quote_background),
            h(c.border),
            h(c.accent)
        )
    };
    format!(
        ":root {{ {} }}
@media (prefers-color-scheme: dark) {{ :root {{ {} }} }}
html {{ background: var(--bg); color: var(--text); }}
body {{ margin: 0; font: 16px/1.6 \"Inter Variable\", Inter, Vazirmatn, system-ui, sans-serif; }}
main {{ max-width: 46rem; margin: 0 auto; padding: 3rem 1.5rem 5rem; }}
/* Each block runs the way its own text does: Persian right to left, English left to right. */
p, li, h1, h2, h3, h4, h5, h6, td, th, blockquote, dt, dd {{ unicode-bidi: plaintext; }}
h1, h2, h3, h4, h5, h6 {{ color: var(--heading); font-weight: 650; line-height: 1.25; margin: 1.8em 0 0.5em; }}
h1 {{ font-size: 1.6em; margin-top: 0; }} h2 {{ font-size: 1.3em; }} h3 {{ font-size: 1.12em; }}
h4 {{ font-size: 1em; }} h5, h6 {{ font-size: 1em; color: var(--dim); }}
a {{ color: var(--link); text-underline-offset: 0.15em; }}
code, pre {{ font-family: \"JetBrains Mono\", Vazirmatn, monospace; font-size: 0.9em; }}
code {{ color: var(--code); background: var(--code-bg); padding: 0.1em 0.3em; border-radius: 4px; }}
pre {{ background: var(--code-bg); padding: 0.8em 1em; border-radius: 8px; overflow-x: auto; direction: ltr; }}
pre code {{ padding: 0; background: none; }}
blockquote {{ margin: 1em 0; padding: 0.2em 1em; border-inline-start: 3px solid var(--accent); color: var(--quote); background: var(--quote-bg); border-radius: 0 6px 6px 0; }}
table {{ border-collapse: collapse; margin: 1em 0; }}
th, td {{ text-align: start; padding: 0.4em 1.4em 0.4em 0; border-bottom: 1px solid var(--border); }}
th {{ color: var(--dim); font-weight: 600; }}
img {{ max-width: 100%; border-radius: 6px; }}
hr {{ border: none; border-top: 1px solid var(--border); margin: 2em 0; }}
li:has(> input[type=checkbox]) {{ list-style: none; margin-inline-start: -1.4em; }}
input[type=checkbox] {{ accent-color: var(--accent); margin-inline-end: 0.5em; }}
",
        colors(light),
        colors(dark)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colors() -> Colors {
        Colors::from(&crate::palettes::built_in()[0])
    }

    #[test]
    fn a_note_becomes_a_page_without_its_front_matter() {
        let page = page("Plan & co", "---\ntags: [a]\n---\n# Weekly plan\n\n- [x] done\n", None, &|_| None, &colors(), &colors());
        assert!(page.contains("<title>Plan &amp; co</title>"));
        assert!(page.contains("<h1>Weekly plan</h1>"));
        assert!(page.contains("checkbox"));
        assert!(!page.contains("tags: [a]"));
    }

    #[test]
    fn a_picture_beside_the_note_is_carried_in_the_page() {
        let dir = std::env::temp_dir().join(format!("nevis-html-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dot.png"), b"PNG").unwrap();
        let page = page("x", "![a dot](dot.png) ![web](https://example.com/a.png)", Some(&dir), &|_| None, &colors(), &colors());
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(page.contains("src=\"data:image/png;base64,UE5H\""));
        assert!(page.contains("src=\"https://example.com/a.png\""));
    }
}
