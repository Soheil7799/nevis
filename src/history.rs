//! Earlier versions of each note, kept on this machine only (in nevis's state folder): each
//! save keeps one (and the text it replaces, when no version has it: a note never saved by
//! nevis, or changed outside it). They thin out as they age: the first of every minute for an
//! hour (autosaving makes many), of every ten minutes for a day, then of every hour; at most 200.

use std::path::{Path, PathBuf};

use gtk::glib;

const MINUTE: i64 = 60 * 1000;
const KEPT: usize = 200;

/// The folder a note's versions are in: named by a checksum of its path, so that any path
/// fits; `path` in it says whose they are.
fn folder(note: &Path) -> PathBuf {
    let sum = glib::compute_checksum_for_string(glib::ChecksumType::Sha256, note.to_string_lossy().as_ref()).unwrap_or_default();
    crate::positions::state_dir().join("history").join(&sum[..sum.len().min(24)])
}

/// Keeps `text` as the version of `note` saved now; `before`, what the file held until then,
/// is kept first if the newest version isn't it.
pub fn keep(note: &Path, before: Option<&str>, text: &str) {
    let folder = folder(note);
    let versions = versions(note);
    let newest = versions.first().and_then(|(newest, _)| std::fs::read_to_string(newest).ok());
    if newest.as_deref() == Some(text) || std::fs::create_dir_all(&folder).is_err() {
        return;
    }
    let now = glib::real_time() / 1000;
    let mut times: Vec<i64> = versions.iter().map(|(_, time)| *time).collect();
    let write = |time: i64, text: &str| std::fs::write(folder.join(format!("{time}.md")), text).is_ok();
    if let Some(before) = before.filter(|before| *before != text && newest.as_deref() != Some(before))
        && write(now - 1, before)
    {
        times.insert(0, now - 1);
    }
    if !write(now, text) {
        return;
    }
    times.insert(0, now);
    let _ = std::fs::write(folder.join("path"), note.to_string_lossy().as_bytes());
    for time in thinned(&times, now) {
        let _ = std::fs::remove_file(folder.join(format!("{time}.md")));
    }
}

/// Which of the versions saved at `times` (ms, newest first) go at `now`: all but the first
/// of their minute in the last hour, of their ten minutes in the day, of their hour before;
/// and the oldest beyond 200.
fn thinned(times: &[i64], now: i64) -> Vec<i64> {
    let mut dropped = Vec::new();
    let mut kept = 0usize;
    // Oldest first, so that each stretch keeps its first.
    let mut last_stretch = None;
    for &time in times.iter().rev() {
        let age = now - time;
        let stretch = match age {
            _ if age < 60 * MINUTE => Some((1, time.div_euclid(MINUTE))),
            _ if age < 24 * 60 * MINUTE => Some((10, time.div_euclid(10 * MINUTE))),
            _ => Some((60, time.div_euclid(60 * MINUTE))),
        };
        if stretch.is_some() && stretch == last_stretch {
            dropped.push(time);
        } else {
            kept += 1;
        }
        last_stretch = stretch;
    }
    // The oldest of those kept, past the last 100.
    let excess = kept.saturating_sub(KEPT);
    let mut oldest: Vec<i64> = times.iter().rev().copied().filter(|time| !dropped.contains(time)).take(excess).collect();
    dropped.append(&mut oldest);
    dropped
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
    let day = |date: &glib::DateTime| date.ymd();
    let yesterday = now.add_days(-1).ok();
    let format = |pattern: &str| then.format(pattern).map(|s| s.to_string()).unwrap_or_default();
    if day(&then) == day(&now) {
        format("Today %H:%M")
    } else if yesterday.is_some_and(|yesterday| day(&yesterday) == day(&then)) {
        format("Yesterday %H:%M")
    } else if then.year() == now.year() {
        format("%a %e %b %H:%M")
    } else {
        format("%e %b %Y %H:%M")
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
    fn in_the_last_hour_the_first_save_of_each_minute_stays() {
        let now = 1000 * MIN;
        assert_eq!(thinned(&[now, now - 4 * MIN, now - 5 * MIN, now - 59 * MIN], now), Vec::<i64>::new());
        assert_eq!(thinned(&[now - 5 * MIN + 2000, now - 5 * MIN + 1000], now), vec![now - 5 * MIN + 2000]);
    }

    #[test]
    fn older_ones_keep_the_first_of_their_ten_minutes_then_of_their_hour() {
        let now = 10_000 * MIN;
        // Two in one ten minutes two hours ago: the later goes.
        assert_eq!(thinned(&[now - 121 * MIN, now - 125 * MIN], now), vec![now - 121 * MIN]);
        // Two in one hour two days ago: the later goes.
        let old = (now - 48 * 60 * MIN).div_euclid(60 * MIN) * 60 * MIN;
        assert_eq!(thinned(&[old + 40 * MIN, old + 5 * MIN], now), vec![old + 40 * MIN]);
    }

    #[test]
    fn only_the_last_two_hundred_stay() {
        let times: Vec<i64> = (0..201).rev().map(|n| n * 60 * MIN).collect();
        assert_eq!(thinned(&times, 200 * 60 * MIN), vec![0]);
    }
}
