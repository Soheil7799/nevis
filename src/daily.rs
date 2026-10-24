//! Today's note (Ctrl+D): `2026-10-08.md` in a folder window, made when first asked for.

/// Where today's note is, relative to the folder. Obsidian's daily-notes settings
/// (`.obsidian/daily-notes.json`) give its folder and name format when there are any;
/// otherwise it goes in a Daily or Journal subfolder if there is one, named 2026-10-08.md.
pub fn path(date: (i32, u32, u32), obsidian: Option<&str>, subfolders: &[&str]) -> String {
    let folder = match obsidian {
        Some(settings) => setting(settings, "folder").unwrap_or_default(),
        None => subfolders
            .iter()
            .find(|name| ["daily", "daily notes", "journal"].contains(&name.to_lowercase().as_str()))
            .map(|name| name.to_string())
            .unwrap_or_default(),
    };
    let (year, month, day) = date;
    let name = obsidian
        .and_then(|settings| setting(settings, "format"))
        .and_then(|format| written(&format, date))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("{year:04}-{month:02}-{day:02}"));
    match folder.trim_matches('/') {
        "" => format!("{name}.md"),
        folder => format!("{folder}/{name}.md"),
    }
}

/// The date written in a moment.js-style `format` ("YYYY-MM-DD", "DD.MM.YY", "[Day] D"), or
/// `None` when it has words nevis doesn't write (weekday and month names).
pub fn written(format: &str, (year, month, day): (i32, u32, u32)) -> Option<String> {
    let mut out = String::new();
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '[' {
            // [Text] is written as it is.
            out.extend(chars.by_ref().take_while(|&c| c != ']'));
            continue;
        }
        if !c.is_ascii_alphabetic() {
            out.push(c);
            continue;
        }
        let mut run = 1;
        while chars.next_if_eq(&c).is_some() {
            run += 1;
        }
        match (c, run) {
            ('Y', 4) => out.push_str(&format!("{year:04}")),
            ('Y', 2) => out.push_str(&format!("{:02}", year.rem_euclid(100))),
            ('M', 1) => out.push_str(&month.to_string()),
            ('M', 2) => out.push_str(&format!("{month:02}")),
            ('D', 1) => out.push_str(&day.to_string()),
            ('D', 2) => out.push_str(&format!("{day:02}")),
            _ => return None,
        }
    }
    Some(out)
}

/// A string setting from Obsidian's (flat) JSON settings: `"key": "value"`.
pub fn setting(json: &str, key: &str) -> Option<String> {
    let quoted = format!("\"{key}\"");
    let after = &json[json.find(&quoted)? + quoted.len()..];
    let mut chars = after.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?.chars();
    let mut value = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(value),
            '\\' => value.push(chars.next()?),
            c => value.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: (i32, u32, u32) = (2026, 10, 8);

    #[test]
    fn todays_note_is_named_by_date_in_a_daily_folder_if_there_is_one() {
        assert_eq!(path(DAY, None, &[]), "2026-10-08.md");
        assert_eq!(path(DAY, None, &["Archive", "daily"]), "daily/2026-10-08.md");
        assert_eq!(path(DAY, None, &["Journal", "Work"]), "Journal/2026-10-08.md");
    }

    #[test]
    fn obsidian_settings_give_the_folder_and_the_name() {
        let settings = r#"{"folder":"Journal/Days","format":"DD.MM.YYYY","template":""}"#;
        assert_eq!(path(DAY, Some(settings), &["Daily"]), "Journal/Days/08.10.2026.md");
        assert_eq!(path(DAY, Some(r#"{"format": "YYYY/MM/YYYY-MM-DD"}"#), &[]), "2026/10/2026-10-08.md");
        // Set to the vault's top, even with a Daily folder there.
        assert_eq!(path(DAY, Some(r#"{"folder": ""}"#), &["Daily"]), "2026-10-08.md");
    }

    #[test]
    fn a_name_format_with_words_nevis_cannot_write_falls_back_to_the_date() {
        assert_eq!(path(DAY, Some(r#"{"format":"dddd, MMMM Do YYYY"}"#), &[]), "2026-10-08.md");
        assert_eq!(path((2026, 1, 5), Some(r#"{"format":"D-M-YY"}"#), &[]), "5-1-26.md");
    }
}
