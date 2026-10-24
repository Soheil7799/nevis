//! The open windows and tabs, kept across runs: nevis comes back as it was left, unsaved
//! notes included (their text lives in draft files beside the session file).

use std::path::PathBuf;

#[derive(Debug, Default, PartialEq)]
pub struct Session {
    pub windows: Vec<WindowState>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowState {
    /// The folder in the sidebar, if any.
    pub folder: Option<PathBuf>,
    /// The tab in front.
    pub active: usize,
    pub tabs: Vec<TabState>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TabState {
    /// The file shown, if it has one.
    pub path: Option<PathBuf>,
    /// The draft holding unsaved text, if any: its name in the drafts folder.
    pub draft: Option<String>,
    pub line: u32,
    pub column: u32,
    /// Showing the rendered view rather than the source.
    pub reading: bool,
    /// Showing the source and the rendered view side by side.
    pub side_by_side: bool,
}

impl Session {
    /// Reads the stored form (see `to_text`).
    pub fn parse(text: &str) -> Session {
        let mut windows: Vec<WindowState> = Vec::new();
        for row in text.lines() {
            let mut fields = row.split('\t');
            match fields.next() {
                Some("window") => {
                    let mut rest = row.splitn(3, '\t').skip(1);
                    let Some(active) = rest.next().and_then(|n| n.parse().ok()) else { continue };
                    let folder = rest.next().map(PathBuf::from).filter(|folder| folder.is_absolute());
                    windows.push(WindowState { folder, active, tabs: Vec::new() });
                }
                Some("tab") => {
                    let mut rest = row.splitn(6, '\t').skip(1);
                    let (Some(view), Some(line), Some(column), Some(draft), Some(path)) =
                        (rest.next(), rest.next(), rest.next(), rest.next(), rest.next())
                    else {
                        continue;
                    };
                    let (Ok(line), Ok(column)) = (line.parse(), column.parse()) else { continue };
                    // Only absolute paths, and only plain draft names: a damaged or edited
                    // file must not make nevis open files elsewhere.
                    let path = Some(PathBuf::from(path)).filter(|path| path.is_absolute());
                    let draft = Some(draft).filter(|draft| is_draft_name(draft)).map(str::to_owned);
                    if path.is_none() && draft.is_none() {
                        continue;
                    }
                    let tab = TabState { path, draft, line, column, reading: view == "r", side_by_side: view == "s" };
                    if windows.is_empty() {
                        windows.push(WindowState { folder: None, active: 0, tabs: Vec::new() });
                    }
                    if let Some(window) = windows.last_mut() {
                        window.tabs.push(tab);
                    }
                }
                _ => {}
            }
        }
        windows.retain(|window| !window.tabs.is_empty());
        for window in &mut windows {
            window.active = window.active.min(window.tabs.len() - 1);
        }
        Session { windows }
    }

    /// One line per window ("window", its active tab, its folder) followed by one per tab
    /// ("tab", r, e or s for the view (rendered, source, side by side), line, column, draft or -, path); tab-separated, with
    /// paths last so that they may hold tabs themselves.
    pub fn to_text(&self) -> String {
        let mut text = String::from("nevis session 1\n");
        for window in &self.windows {
            let folder = window.folder.as_ref().map(|f| f.to_string_lossy()).unwrap_or_default();
            text.push_str(&format!("window\t{}\t{folder}\n", window.active));
            for tab in &window.tabs {
                let view = if tab.side_by_side { "s" } else if tab.reading { "r" } else { "e" };
                let draft = tab.draft.as_deref().unwrap_or("-");
                let path = tab.path.as_ref().map(|p| p.to_string_lossy()).unwrap_or_default();
                text.push_str(&format!("tab\t{view}\t{}\t{}\t{draft}\t{path}\n", tab.line, tab.column));
            }
        }
        text
    }
}

/// Whether sessions are kept: always, except in measurement runs (NEVIS_SESSION=1 there too).
pub fn enabled() -> bool {
    !crate::devhooks::active() || std::env::var_os("NEVIS_SESSION").is_some_and(|v| v == "1")
}

/// The session left last time; empty when there is none.
pub fn load() -> Session {
    Session::parse(&std::fs::read_to_string(session_file()).unwrap_or_default())
}

/// Keeps `session` for next time, and lets go of drafts it no longer has.
pub fn store(session: &Session) {
    write_atomically(&session_file(), &session.to_text());
    let kept: Vec<&str> = session.windows.iter().flat_map(|w| &w.tabs).filter_map(|t| t.draft.as_deref()).collect();
    let Ok(entries) = std::fs::read_dir(drafts_dir()) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_draft_name(&name) && !kept.contains(&name.as_str()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// `windows` as one window: all their tabs in order, a file open in several once (with its
/// unsaved text, if one has some; if two have, the other as a note of its own), the first folder any showed, and the first window's tab in
/// front. A session comes back as tabs, not as a window for every file once opened.
pub fn merged(windows: Vec<WindowState>) -> Option<WindowState> {
    let folder = windows.iter().find_map(|window| window.folder.clone());
    let mut tabs: Vec<TabState> = Vec::new();
    let mut active = None;
    for (n, window) in windows.into_iter().enumerate() {
        for (i, tab) in window.tabs.into_iter().enumerate() {
            let front = n == 0 && i == window.active;
            let twin = tab.path.as_ref().and_then(|path| tabs.iter().position(|t| t.path.as_ref() == Some(path)));
            let at = match twin {
                Some(at) => match (&tabs[at].draft, &tab.draft) {
                    (None, Some(_)) => {
                        tabs[at] = tab;
                        at
                    }
                    // Unsaved text in both: the second comes back as a note of its own,
                    // unsaved, rather than lost.
                    (Some(_), Some(_)) => {
                        tabs.push(TabState { path: None, ..tab });
                        tabs.len() - 1
                    }
                    _ => at,
                },
                None => {
                    tabs.push(tab);
                    tabs.len() - 1
                }
            };
            if front {
                active = Some(at);
            }
        }
    }
    (!tabs.is_empty()).then(|| WindowState { folder, active: active.unwrap_or(0), tabs })
}

/// A name for a new draft.
pub fn new_draft_name() -> String {
    format!("{:08x}{:08x}", gtk::glib::random_int(), gtk::glib::random_int())
}

pub fn write_draft(name: &str, text: &str) {
    if is_draft_name(name) {
        write_atomically(&drafts_dir().join(name), text);
    }
}

pub fn read_draft(name: &str) -> Option<String> {
    is_draft_name(name).then(|| std::fs::read_to_string(drafts_dir().join(name)).ok()).flatten()
}

pub fn remove_draft(name: &str) {
    if is_draft_name(name) {
        let _ = std::fs::remove_file(drafts_dir().join(name));
    }
}

fn session_file() -> PathBuf {
    crate::positions::state_dir().join("session")
}

fn drafts_dir() -> PathBuf {
    crate::positions::state_dir().join("drafts")
}

/// Written aside and renamed into place, so a crash never leaves half a file.
fn write_atomically(path: &std::path::Path, text: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let partial = path.with_extension("partial");
    if std::fs::write(&partial, text).is_ok() {
        let _ = std::fs::rename(&partial, path);
    }
}

/// Draft names are a few hexadecimal digits, nothing that could point outside their folder.
pub fn is_draft_name(name: &str) -> bool {
    (1..=32).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(path: Option<&str>, draft: Option<&str>, line: u32, column: u32, reading: bool) -> TabState {
        TabState { path: path.map(PathBuf::from), draft: draft.map(str::to_owned), line, column, reading, side_by_side: false }
    }

    #[test]
    fn windows_and_tabs_come_back_as_they_were_written() {
        let session = Session {
            windows: vec![
                WindowState {
                    folder: Some("/home/me/Notes".into()),
                    active: 1,
                    tabs: vec![tab(Some("/home/me/Notes/todo.md"), None, 12, 3, true), tab(None, Some("3fa9c2"), 0, 0, false)],
                },
                WindowState { folder: None, active: 0, tabs: vec![tab(Some("/tmp/a b\tc.txt"), Some("77"), 1, 2, false)] },
            ],
        };
        assert_eq!(Session::parse(&session.to_text()), session);
    }

    #[test]
    fn a_tab_side_by_side_comes_back_so() {
        let split = TabState { side_by_side: true, ..tab(Some("/n/a.md"), None, 3, 1, false) };
        let session = Session { windows: vec![WindowState { folder: None, active: 0, tabs: vec![split] }] };
        assert_eq!(Session::parse(&session.to_text()), session);
    }

    fn a_unsaved_too() -> TabState {
        tab(Some("/n/a.md"), Some("77"), 5, 0, false)
    }

    #[test]
    fn a_session_of_many_windows_comes_back_as_one_with_all_their_tabs() {
        let a = tab(Some("/n/a.md"), None, 1, 0, false);
        let b = tab(Some("/n/b.md"), None, 2, 0, true);
        let draft = tab(None, Some("3fa9c2"), 0, 0, false);
        let a_unsaved = tab(Some("/n/a.md"), Some("77"), 5, 0, false);
        let windows = vec![
            WindowState { folder: None, active: 1, tabs: vec![a.clone(), b.clone()] },
            WindowState { folder: Some("/n".into()), active: 0, tabs: vec![draft.clone(), a_unsaved.clone()] },
            WindowState { folder: Some("/other".into()), active: 0, tabs: vec![b.clone()] },
        ];
        // The first folder any had; a file open twice once, its unsaved text kept; the first
        // window's tab in front.
        assert_eq!(
            merged(windows),
            Some(WindowState { folder: Some("/n".into()), active: 1, tabs: vec![a_unsaved, b, draft] })
        );
        assert_eq!(merged(Vec::new()), None);
        // Unsaved text in two windows: none of it lost.
        let other = tab(Some("/n/a.md"), Some("88"), 1, 0, false);
        let both = vec![
            WindowState { folder: None, active: 0, tabs: vec![a_unsaved_too()] },
            WindowState { folder: None, active: 0, tabs: vec![other] },
        ];
        let tabs = merged(both).unwrap().tabs;
        assert_eq!(tabs.iter().filter_map(|t| t.draft.as_deref()).collect::<Vec<_>>(), ["77", "88"]);
        assert_eq!(tabs[1].path, None);
    }

    #[test]
    fn damaged_or_unsafe_lines_are_skipped_and_the_rest_kept() {
        let text = "nevis session 1
window\t0\t/home/me
tab\tr\t1\t2\t-\t/home/me/a.md
this line is garbage
tab\tr\tx\t2\t-\t/home/me/bad-number.md
tab\te\t0\t0\t../../etc/passwd\t
tab\te\t0\t0\t-\trelative/path.md
tab\te\t0\t0\t-\t
tab\tr\t5\t6\t-\t/home/me/b.md
";
        let tabs = &Session::parse(text).windows[0].tabs;
        assert_eq!(tabs, &[tab(Some("/home/me/a.md"), None, 1, 2, true), tab(Some("/home/me/b.md"), None, 5, 6, true)]);
    }

    #[test]
    fn windows_are_made_whole() {
        let text = "tab\te\t0\t0\t-\t/home/me/early.md
window\t9\t
tab\te\t0\t0\t-\t/home/me/x.md
window\t0\t/home/me/empty
";
        let session = Session::parse(text);
        let shape: Vec<(usize, usize)> = session.windows.iter().map(|w| (w.tabs.len(), w.active)).collect();
        // Early tabs get a window of their own; the active tab is one that exists; a window
        // left without tabs is dropped.
        assert_eq!(shape, [(1, 0), (1, 0)]);
    }
}
