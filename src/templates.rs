//! New notes from templates: the notes in a folder window's templates folder, their
//! `{{title}}`, `{{date}}` and `{{time}}` filled in as Obsidian's Templates plugin does.

use crate::daily;

/// The templates folder, relative to the window's folder: the one Obsidian's settings
/// (`.obsidian/templates.json`) name, else a subfolder called templates (any case).
pub fn folder(obsidian: Option<&str>, subfolders: &[&str]) -> Option<String> {
    obsidian
        .and_then(|settings| daily::setting(settings, "folder"))
        .map(|folder| folder.trim_matches('/').to_owned())
        .filter(|folder| !folder.is_empty())
        .or_else(|| subfolders.iter().find(|name| ["templates", "_templates"].contains(&name.to_lowercase().as_str())).map(|name| name.to_string()))
}

/// `template` for a note called `title`, made at `date` (year, month, day) and `time` (hours,
/// minutes): `{{title}}`, `{{date}}` (2026-10-24), `{{time}}` (18:05), and `{{date:FORMAT}}`
/// in the formats daily notes take. Anything else stays as written.
pub fn fill(template: &str, title: &str, date: (i32, u32, u32), time: (u32, u32)) -> String {
    let (year, month, day) = date;
    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            rest = &rest[open..];
            break;
        };
        let name = after[..close].trim();
        let filled = match name.split_once(':') {
            _ if name == "title" => Some(title.to_owned()),
            _ if name == "date" => Some(format!("{year:04}-{month:02}-{day:02}")),
            _ if name == "time" => Some(format!("{:02}:{:02}", time.0, time.1)),
            Some(("date", format)) => daily::written(format.trim(), date),
            _ => None,
        };
        match filled {
            Some(filled) => out.push_str(&filled),
            None => out.push_str(&rest[open..open + 2 + close + 2]),
        }
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obsidians_folder_else_a_templates_subfolder() {
        assert_eq!(folder(Some(r#"{"folder":"Meta/Templates"}"#), &["templates"]).as_deref(), Some("Meta/Templates"));
        assert_eq!(folder(None, &["Daily", "Templates"]).as_deref(), Some("Templates"));
        assert_eq!(folder(None, &["Daily"]), None);
    }

    #[test]
    fn title_date_and_time_are_filled_in_and_the_rest_left() {
        let template = "# {{title}}\n\nMade {{ date }} at {{time}}, {{date:DD.MM.YYYY}}; {{unknown}} {{open";
        assert_eq!(
            fill(template, "Trip", (2026, 10, 4), (9, 5)),
            "# Trip\n\nMade 2026-10-04 at 09:05, 04.10.2026; {{unknown}} {{open"
        );
    }
}
