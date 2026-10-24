//! Earlier versions of each note, kept on this machine only (in nevis's state folder): each
//! save keeps one, at most one every ten minutes while the note is written (a later save in
//! those ten minutes takes its place), the last 100.

use std::path::{Path, PathBuf};

use gtk::glib;

/// Saves this soon after a version's first share it: the later one takes its place.
const APART_MS: i64 = 10 * 60 * 1000;
const KEPT: usize = 100;

/// The folder a note's versions are in: named by a checksum of its path, so that any path
/// fits; `path` in it says whose they are.
fn folder(note: &Path) -> PathBuf {
    let sum = glib::compute_checksum_for_string(glib::ChecksumType::Sha256, note.to_string_lossy().as_ref()).unwrap_or_default();
    crate::positions::state_dir().join("history").join(&sum[..sum.len().min(24)])
}

/// Keeps `text` as the version of `note` saved now.
pub fn keep(note: &Path, text: &str) {
    let folder = folder(note);
    let versions = versions(note);
    if let Some((newest, _)) = versions.first()
        && std::fs::read_to_string(newest).is_ok_and(|kept| kept == text)
    {
        return;
    }
    let now = glib::real_time() / 1000;
    let times: Vec<i64> = versions.iter().map(|(_, time)| *time).collect();
    let (name, dropped) = plan(&times, now);
    if std::fs::create_dir_all(&folder).is_err() || std::fs::write(folder.join(format!("{name}.md")), text).is_err() {
        return;
    }
    let _ = std::fs::write(folder.join("path"), note.to_string_lossy().as_bytes());
    for time in dropped {
        let _ = std::fs::remove_file(folder.join(format!("{time}.md")));
    }
}

/// Where a version saved at `now` (ms) goes, among those started at `times` (newest first):
/// in the newest, if that one started less than ten minutes ago, else a new one started now;
/// and the oldest it pushes out of the last 100.
fn plan(times: &[i64], now: i64) -> (i64, Vec<i64>) {
    match times.first() {
        Some(&newest) if now - newest < APART_MS => (newest, Vec::new()),
        _ => (now, times.iter().rev().take((times.len() + 1).saturating_sub(KEPT)).copied().collect()),
    }
}

/// `note`'s versions, newest first: each file, and when it started (ms since 1970; its file's
/// time says when it was saved last).
pub fn versions(note: &Path) -> Vec<(PathBuf, i64)> {
    let Ok(entries) = std::fs::read_dir(folder(note)) else { return Vec::new() };
    let mut versions: Vec<(PathBuf, i64)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let time = path.file_stem()?.to_str()?.parse().ok()?;
            (path.extension()? == "md").then_some((path, time))
        })
        .collect();
    versions.sort_by_key(|(_, time)| std::cmp::Reverse(*time));
    versions
}

/// "Today 18:05", "Yesterday 09:30", "Mon 12 Oct 14:00", "12 Oct 2025 14:00".
pub fn when(time_ms: i64) -> String {
    let Ok(then) = glib::DateTime::from_unix_local(time_ms / 1000) else { return String::new() };
    let Ok(now) = glib::DateTime::now_local() else { return String::new() };
    let days = |date: &glib::DateTime| date.year() * 400 + date.day_of_year();
    let format = |pattern: &str| then.format(pattern).map(|s| s.to_string()).unwrap_or_default();
    match days(&now) - days(&then) {
        0 => format("Today %H:%M"),
        1 => format("Yesterday %H:%M"),
        _ if then.year() == now.year() => format("%a %e %b %H:%M"),
        _ => format("%e %b %Y %H:%M"),
    }
}

/// A note (or a folder of notes) moved from `from` to `to`: its versions go with it.
pub fn moved(from: &Path, to: &Path) {
    let Ok(entries) = std::fs::read_dir(crate::positions::state_dir().join("history")) else { return };
    for entry in entries.flatten() {
        let Ok(owner) = std::fs::read_to_string(entry.path().join("path")) else { continue };
        let Ok(below) = Path::new(&owner).strip_prefix(from) else { continue };
        let note = if below.as_os_str().is_empty() { to.to_path_buf() } else { to.join(below) };
        let target = folder(&note);
        if target.exists() || std::fs::rename(entry.path(), &target).is_err() {
            continue;
        }
        let _ = std::fs::write(target.join("path"), note.to_string_lossy().as_bytes());
    }
}

/// A line of what going back to a version changes.
#[derive(Debug, PartialEq)]
pub enum Line<'a> {
    Same(&'a str),
    /// In the version, not in the note now: it comes back.
    Back(&'a str),
    /// In the note now, not in the version: it goes.
    Gone(&'a str),
}

/// What changes from `now` to `version`, line by line (the longest run of lines both share
/// kept); `None` when they are too long to compare quickly.
pub fn changes<'a>(now: &'a str, version: &'a str) -> Option<Vec<Line<'a>>> {
    let (a, b): (Vec<&str>, Vec<&str>) = (now.lines().collect(), version.lines().collect());
    // The lines both start and end with, set aside: usually most of a note.
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..].iter().rev().zip(b[head..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (x, y) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);
    if x.len().saturating_mul(y.len()) > 4_000_000 {
        return None;
    }
    // longest[i][j]: the most lines x[i..] and y[j..] share, in order.
    let mut longest = vec![vec![0u32; y.len() + 1]; x.len() + 1];
    for i in (0..x.len()).rev() {
        for j in (0..y.len()).rev() {
            longest[i][j] = if x[i] == y[j] { longest[i + 1][j + 1] + 1 } else { longest[i + 1][j].max(longest[i][j + 1]) };
        }
    }
    let mut lines: Vec<Line> = a[..head].iter().map(|line| Line::Same(line)).collect();
    let (mut i, mut j) = (0, 0);
    while i < x.len() || j < y.len() {
        if i < x.len() && j < y.len() && x[i] == y[j] {
            lines.push(Line::Same(x[i]));
            (i, j) = (i + 1, j + 1);
        } else if j < y.len() && (i == x.len() || longest[i][j + 1] >= longest[i + 1][j]) {
            lines.push(Line::Back(y[j]));
            j += 1;
        } else {
            lines.push(Line::Gone(x[i]));
            i += 1;
        }
    }
    lines.extend(a[a.len() - tail..].iter().map(|line| Line::Same(line)));
    Some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn going_back_shows_the_lines_that_come_back_and_those_that_go() {
        let now = "# Plan\nkeep\nnew line\nend";
        let version = "# Plan\nold line\nkeep\nend";
        assert_eq!(
            changes(now, version),
            Some(vec![Line::Same("# Plan"), Line::Back("old line"), Line::Same("keep"), Line::Gone("new line"), Line::Same("end")])
        );
        assert_eq!(changes("same", "same"), Some(vec![Line::Same("same")]));
    }

    const MIN: i64 = 60 * 1000;

    #[test]
    fn a_save_within_ten_minutes_of_the_newest_versions_start_takes_its_place() {
        assert_eq!(plan(&[100 * MIN, 50 * MIN], 105 * MIN), (100 * MIN, vec![]));
        assert_eq!(plan(&[100 * MIN], 111 * MIN), (111 * MIN, vec![]));
        assert_eq!(plan(&[], 7), (7, vec![]));
    }

    #[test]
    fn only_the_last_hundred_stay() {
        let times: Vec<i64> = (0..100).rev().map(|n| n * 20 * MIN).collect();
        assert_eq!(plan(&times, 100 * 20 * MIN), (100 * 20 * MIN, vec![0]));
    }
}
