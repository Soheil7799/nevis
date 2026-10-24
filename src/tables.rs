//! Markdown tables as you edit them: Tab and Shift+Tab go from cell to cell, Enter starts a
//! row, and the columns line up each time.

use crate::render::display_width;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Align {
    None,
    Left,
    Center,
    Right,
}

/// A table: its rows of cells (the header first), and how each column is aligned.
#[derive(Debug, PartialEq)]
pub struct Table {
    rows: Vec<Vec<String>>,
    aligns: Vec<Align>,
}

/// Where Tab, Shift+Tab or Enter go.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Move {
    Next,
    Previous,
    NewRow,
}

/// The table's lines around line `at` of `lines`: the first and last (inclusive), when `at`
/// is in a table (lines with pipes, the second its divider, "|---|:-:|").
pub fn around(lines: &[&str], at: usize) -> Option<(usize, usize)> {
    let piped = |i: usize| lines.get(i).is_some_and(|line| !line.trim().is_empty() && line.contains('|'));
    if !piped(at) {
        return None;
    }
    let (mut first, mut last) = (at, at);
    while first > 0 && piped(first - 1) {
        first -= 1;
    }
    while piped(last + 1) {
        last += 1;
    }
    // A run of piped lines may start before the table; its divider tells where the header is.
    let divider = (first + 1..=last).find(|&i| is_divider(lines[i]))?;
    let first = divider - 1;
    (at >= first).then_some((first, last))
}

fn is_divider(line: &str) -> bool {
    let cells = cells(line);
    !cells.is_empty() && cells.iter().all(|cell| {
        let dashes = cell.trim_matches(':');
        !dashes.is_empty() && dashes.chars().all(|c| c == '-')
    })
}

/// A row's cells, trimmed; the pipes at its ends are optional, and `\|` is no pipe.
fn cells(line: &str) -> Vec<String> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let line = if line.ends_with('|') && !line.ends_with("\\|") { &line[..line.len() - 1] } else { line };
    let mut cells = vec![String::new()];
    let mut escaped = false;
    for c in line.chars() {
        if c == '|' && !escaped {
            cells.push(String::new());
        } else if let Some(cell) = cells.last_mut() {
            cell.push(c);
        }
        escaped = c == '\\' && !escaped;
    }
    cells.into_iter().map(|cell| cell.trim().to_owned()).collect()
}

/// Which cell of `line` the character `column` is in.
pub fn cell_at(line: &str, column: usize) -> usize {
    let lead = line.trim_start().starts_with('|');
    let mut pipes = 0usize;
    let mut escaped = false;
    for c in line.chars().take(column) {
        if c == '|' && !escaped {
            pipes += 1;
        }
        escaped = c == '\\' && !escaped;
    }
    if lead { pipes.saturating_sub(1) } else { pipes }
}

impl Table {
    pub fn parse(lines: &[&str]) -> Option<Table> {
        let (header, rest) = lines.split_first()?;
        let (divider, body) = rest.split_first()?;
        if !is_divider(divider) {
            return None;
        }
        let aligns: Vec<Align> = cells(divider)
            .iter()
            .map(|cell| match (cell.starts_with(':'), cell.ends_with(':')) {
                (true, true) => Align::Center,
                (true, false) => Align::Left,
                (false, true) => Align::Right,
                (false, false) => Align::None,
            })
            .collect();
        let rows: Vec<Vec<String>> = std::iter::once(header).chain(body).map(|line| cells(line)).collect();
        let width = rows.iter().map(Vec::len).chain([aligns.len()]).max().unwrap_or(1);
        let mut table = Table { rows, aligns };
        table.aligns.resize(width, Align::None);
        for row in &mut table.rows {
            row.resize(width, String::new());
        }
        Some(table)
    }

    /// The table written with its columns lined up: a line for each row, the divider second.
    pub fn lines(&self) -> Vec<String> {
        let widths: Vec<usize> = (0..self.aligns.len())
            .map(|col| self.rows.iter().map(|row| display_width(&row[col])).max().unwrap_or(0).max(3))
            .collect();
        let row = |cells: &[String]| {
            let mut line = String::from("|");
            for ((cell, &width), align) in cells.iter().zip(&widths).zip(&self.aligns) {
                let room = width - display_width(cell);
                let (before, after) = match align {
                    Align::Right => (room, 0),
                    Align::Center => (room / 2, room - room / 2),
                    _ => (0, room),
                };
                line.push_str(&format!(" {}{cell}{} |", " ".repeat(before), " ".repeat(after)));
            }
            line
        };
        let divider = {
            let mut line = String::from("|");
            for (&width, align) in widths.iter().zip(&self.aligns) {
                let (left, right) = match align {
                    Align::Left => (":", "-"),
                    Align::Center => (":", ":"),
                    Align::Right => ("-", ":"),
                    Align::None => ("-", "-"),
                };
                line.push_str(&format!(" {left}{}{right} |", "-".repeat(width - 2)));
            }
            line
        };
        let mut lines = vec![row(&self.rows[0]), divider];
        lines.extend(self.rows[1..].iter().map(|cells| row(cells)));
        lines
    }

    /// The table after `how` from the cell at `line` (of its lines, the divider counted) and
    /// `cell`: its lines, and where the cursor goes, as a line and a character range in it
    /// (the cell's text, to type over).
    pub fn step(mut self, line: usize, cell: usize, how: Move) -> (Vec<String>, usize, std::ops::Range<usize>) {
        let columns = self.aligns.len();
        // Rows of `self.rows`: the header is 0, the divider has none.
        let row = if line == 0 { 0 } else { line.saturating_sub(1).max(1).min(self.rows.len()) };
        let cell = cell.min(columns - 1);
        let (row, cell) = match how {
            Move::Next if cell + 1 < columns => (row, cell + 1),
            Move::Next | Move::NewRow => {
                if how == Move::NewRow || row + 1 == self.rows.len() {
                    self.rows.insert(row + 1, vec![String::new(); columns]);
                }
                (row + 1, 0)
            }
            Move::Previous if cell > 0 => (row, cell - 1),
            Move::Previous if row > 0 => (row - 1, columns - 1),
            Move::Previous => (0, 0),
        };
        let lines = self.lines();
        let at = if row == 0 { 0 } else { row + 1 };
        let range = cell_text(&lines[at], cell);
        (lines, at, range)
    }
}

/// Where cell `cell`'s text is in a line `lines` wrote: a character range (empty, after its
/// space, for an empty cell).
fn cell_text(line: &str, cell: usize) -> std::ops::Range<usize> {
    let chars: Vec<char> = line.chars().collect();
    let pipes: Vec<usize> = (0..chars.len()).filter(|&i| chars[i] == '|' && (i == 0 || chars[i - 1] != '\\')).collect();
    let (Some(&open), Some(&close)) = (pipes.get(cell), pipes.get(cell + 1)) else { return 0..0 };
    let inside = open + 1..close;
    let start = inside.clone().find(|&i| chars[i] != ' ').unwrap_or(open + 2);
    let end = inside.rev().find(|&i| chars[i] != ' ').map_or(start, |i| i + 1).max(start);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &[&str] = &["| Page | Status |", "|---|:-:|", "| Home | done |", "| Blog | moving |"];

    #[test]
    fn a_table_is_found_from_any_of_its_lines_and_text_around_it_is_not() {
        let lines = ["Intro | with a pipe", "", "a | b", "--|--", "1 | 2", "", "after"];
        assert_eq!(around(&lines, 4), Some((2, 4)));
        assert_eq!(around(&lines, 2), Some((2, 4)));
        assert_eq!(around(&lines, 0), None);
        assert_eq!(around(&lines, 6), None);
    }

    #[test]
    fn columns_line_up_and_keep_their_alignment() {
        let table = Table::parse(TABLE).unwrap();
        assert_eq!(table.lines(), ["| Page | Status |", "| ---- | :----: |", "| Home |  done  |", "| Blog | moving |"]);
    }

    #[test]
    fn a_short_row_gets_its_missing_cells_and_escaped_pipes_stay_in_their_cell() {
        let table = Table::parse(&["a | b | c", "---|---|---", "x \\| y"]).unwrap();
        assert_eq!(table.lines()[2], "| x \\| y |     |     |");
    }

    #[test]
    fn tab_goes_to_the_next_cell_then_the_next_row_then_a_new_one() {
        let next = |line, cell| Table::parse(TABLE).unwrap().step(line, cell, Move::Next);
        let (_, line, range) = next(0, 0);
        assert_eq!((line, range), (0, 9..15));
        let (_, line, range) = next(2, 1);
        assert_eq!((line, range), (3, 2..6));
        let (lines, line, range) = next(3, 1);
        assert_eq!(lines.len(), 5);
        assert_eq!((line, range), (4, 2..2));
    }

    #[test]
    fn shift_tab_goes_back_and_enter_starts_a_row_under_the_cursors() {
        let (_, line, range) = Table::parse(TABLE).unwrap().step(2, 0, Move::Previous);
        assert_eq!((line, range), (0, 9..15));
        let (lines, line, _) = Table::parse(TABLE).unwrap().step(2, 1, Move::NewRow);
        assert_eq!(lines[3], "|      |        |");
        assert_eq!(line, 3);
    }

    #[test]
    fn the_cursors_cell_counts_the_pipes_before_it() {
        assert_eq!(cell_at("| a | b |", 2), 0);
        assert_eq!(cell_at("| a | b |", 6), 1);
        assert_eq!(cell_at("a | b", 4), 1);
        assert_eq!(cell_at("| a \\| b | c |", 8), 0);
    }
}
