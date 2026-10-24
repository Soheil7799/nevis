//! How long a note is: its words and the minutes they take to read.

/// Words: runs of letters or digits between spaces; Markdown's marks ("#", "-", "**") alone
/// are no words.
pub fn count(text: &str) -> usize {
    text.split_whitespace().filter(|word| word.chars().any(char::is_alphanumeric)).count()
}

/// "1,234 words · 6 min read" (none under a minute's worth).
pub fn summary(text: &str) -> String {
    let words = count(text);
    let number = grouped(words);
    let noun = if words == 1 { "word" } else { "words" };
    match words.div_ceil(230) {
        0 => format!("{number} {noun}"),
        minutes => format!("{number} {noun} · {minutes} min read"),
    }
}

fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_alone_is_no_words_and_persian_counts() {
        assert_eq!(count("# Title\n\n- [ ] buy **milk** — now\n"), 4);
        assert_eq!(count("سلام دنیا، hello"), 3);
        assert_eq!(count(""), 0);
    }

    #[test]
    fn the_summary_says_the_words_and_the_minutes() {
        assert_eq!(summary(""), "0 words");
        assert_eq!(summary("one"), "1 word · 1 min read");
        assert_eq!(summary(&"word ".repeat(1234)), "1,234 words · 6 min read");
    }
}
