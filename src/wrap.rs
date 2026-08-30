/// Word-wraps prose text to the terminal width so long lines break at
/// whitespace instead of being hard-wrapped mid-word by the terminal.
/// Existing line breaks (paragraphs, lists, code blocks) are preserved —
/// each line is wrapped independently rather than reflowed into one blob.
/// Falls back to 80 columns when the width can't be determined (e.g.
/// output piped to a file).
pub fn wrap(text: &str) -> String {
    let width = textwrap::termwidth().max(20);
    text.split('\n')
        .map(|line| textwrap::fill(line, width))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_long_line_at_word_boundaries() {
        let text = "one two three four five six seven eight nine ten";
        let wrapped = textwrap::fill(text, 20);
        for line in wrapped.lines() {
            assert!(line.len() <= 20);
        }
        // Sanity: no word got split (every wrapped line's words are a
        // subsequence of the original text's words).
        let words: Vec<&str> = text.split_whitespace().collect();
        let wrapped_words: Vec<&str> = wrapped.split_whitespace().collect();
        assert_eq!(words, wrapped_words);
    }

    #[test]
    fn preserves_existing_line_breaks() {
        let text = "line one\nline two\n\nline three";
        let wrapped = wrap(text);
        assert_eq!(wrapped.lines().count(), 4);
        assert_eq!(wrapped.lines().nth(2), Some(""));
    }
}
