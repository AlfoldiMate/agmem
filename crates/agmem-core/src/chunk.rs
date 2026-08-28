//! Episode chunking: turning verbatim text into retrieval-sized pieces.
//!
//! Chunks are what search actually matches, so they are cut at boundaries a
//! reader would recognise — paragraph first, sentence if a paragraph is too
//! long, and only then at a word. Nothing is dropped: rejoining the chunks
//! reproduces the input up to whitespace, which is what makes the episode row
//! and its chunks two views of the same text rather than two texts.

/// Chunk size to aim for, in characters.
///
/// Around 1500 characters is a few hundred tokens: long enough for a claim to
/// keep its context, short enough that an embedding still points somewhere.
pub const TARGET_CHARS: usize = 1500;

/// Cut `text` into ordered chunks.
///
/// Each chunk stays within [`TARGET_CHARS`], with one exception: a single
/// unbroken run of non-whitespace longer than the budget (a base64 blob, a
/// minified line) has no boundary to cut at and becomes one oversized chunk.
/// Whitespace-only input yields no chunks.
///
/// ```
/// use agmem_core::chunk;
/// let chunks = chunk::chunk("First para.\n\nSecond para.");
/// assert_eq!(chunks, vec!["First para. Second para."]);
/// ```
pub fn chunk(text: &str) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut taken = 0usize;

    for unit in units(text) {
        let length = unit.chars().count();
        if taken > 0 && taken + 1 + length > TARGET_CHARS {
            chunks.push(std::mem::take(&mut current));
            taken = 0;
        }
        if taken > 0 {
            current.push(' ');
            taken += 1;
        }
        current.push_str(unit);
        taken += length;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// The pieces chunks get packed from: paragraphs, split further only when one
/// cannot fit the budget on its own.
fn units(text: &str) -> Vec<&str> {
    let mut units = Vec::new();
    for paragraph in paragraphs(text) {
        if paragraph.chars().count() <= TARGET_CHARS {
            units.push(paragraph);
            continue;
        }
        for sentence in sentences(paragraph) {
            if sentence.chars().count() <= TARGET_CHARS {
                units.push(sentence);
            } else {
                units.extend(wrap_words(sentence));
            }
        }
    }
    units
}

/// Runs of consecutive non-blank lines, trimmed.
fn paragraphs(text: &str) -> Vec<&str> {
    let mut paragraphs = Vec::new();
    let mut start: Option<usize> = None;
    let mut offset = 0usize;

    for line in text.split_inclusive('\n') {
        if line.trim().is_empty() {
            if let Some(begin) = start.take() {
                paragraphs.push(text[begin..offset].trim());
            }
        } else if start.is_none() {
            start = Some(offset);
        }
        offset += line.len();
    }
    if let Some(begin) = start {
        paragraphs.push(text[begin..].trim());
    }
    paragraphs.retain(|paragraph| !paragraph.is_empty());
    paragraphs
}

/// Split after `.`, `!` or `?` when whitespace follows.
///
/// Abbreviations ("e.g. this") therefore split too — the cost is a slightly
/// short chunk, and only inside a paragraph already too long to keep whole.
fn sentences(paragraph: &str) -> Vec<&str> {
    let mut sentences = Vec::new();
    let mut start = 0usize;
    let mut chars = paragraph.char_indices().peekable();

    while let Some((index, character)) = chars.next() {
        if !matches!(character, '.' | '!' | '?') {
            continue;
        }
        let Some(&(next_index, next)) = chars.peek() else {
            continue;
        };
        if !next.is_whitespace() {
            continue;
        }
        let sentence = paragraph[start..index + character.len_utf8()].trim();
        if !sentence.is_empty() {
            sentences.push(sentence);
        }
        start = next_index;
    }
    let tail = paragraph[start..].trim();
    if !tail.is_empty() {
        sentences.push(tail);
    }
    sentences
}

/// Last resort: pack whole words up to the budget.
///
/// The budget counts the whitespace *as it stands in the text*, not one space
/// per gap — a piece keeps its original spacing, so a paragraph padded with
/// newlines would otherwise sail past the limit.
fn wrap_words(text: &str) -> Vec<&str> {
    let mut wrapped = Vec::new();
    let mut start: Option<usize> = None;
    let mut end = 0usize;
    let mut taken = 0usize;

    for (offset, word) in words(text) {
        let length = word.chars().count();
        match start {
            Some(begin) => {
                let gap = text[end..offset].chars().count();
                if taken + gap + length > TARGET_CHARS {
                    wrapped.push(&text[begin..end]);
                    start = Some(offset);
                    taken = length;
                } else {
                    taken += gap + length;
                }
            }
            None => {
                start = Some(offset);
                taken = length;
            }
        }
        end = offset + word.len();
    }
    if let Some(begin) = start {
        wrapped.push(&text[begin..end]);
    }
    wrapped
}

/// Whitespace-separated words with their byte offsets.
fn words(text: &str) -> Vec<(usize, &str)> {
    let mut words = Vec::new();
    let mut start: Option<usize> = None;

    for (index, character) in text.char_indices() {
        if character.is_whitespace() {
            if let Some(begin) = start.take() {
                words.push((begin, &text[begin..index]));
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(begin) = start {
        words.push((begin, &text[begin..]));
    }
    words
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Whitespace-collapsed form; the chunker is only lossless up to this.
    fn squashed(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn short_text_is_one_chunk() {
        assert_eq!(chunk("a short episode"), vec!["a short episode"]);
        assert!(chunk("   \n\t  ").is_empty());
        assert!(chunk("").is_empty());
    }

    #[test]
    fn paragraphs_pack_together_until_the_budget() {
        let paragraph = "x".repeat(900);
        let text = format!("{paragraph}\n\n{paragraph}\n\n{paragraph}");
        let chunks = chunk(&text);

        assert_eq!(chunks.len(), 3, "900-char paragraphs cannot pair up");
        let text = format!("{}\n\n{}", "y".repeat(700), "z".repeat(700));
        assert_eq!(chunk(&text).len(), 1, "but 700-char ones can");
    }

    #[test]
    fn an_overlong_paragraph_is_cut_at_sentences() {
        let sentence = format!("{}. ", "word".repeat(100));
        let text = sentence.repeat(6);
        let chunks = chunk(&text);

        assert!(chunks.len() > 1, "3000 chars must split");
        for piece in &chunks {
            assert!(piece.chars().count() <= TARGET_CHARS, "{}", piece.len());
            assert!(piece.ends_with('.'), "no mid-sentence cuts: {piece:?}");
        }
    }

    #[test]
    fn an_unbreakable_run_becomes_its_own_oversized_chunk() {
        let blob = "b".repeat(TARGET_CHARS * 2);
        let chunks = chunk(&format!("intro.\n\n{blob}\n\noutro."));

        assert!(chunks.iter().any(|piece| piece == &blob));
        assert_eq!(
            squashed(&chunks.join(" ")),
            squashed(&format!("intro. {blob} outro."))
        );
    }

    proptest! {
        #[test]
        fn chunking_never_panics_and_respects_the_budget(text in "(?s).{0,4000}") {
            for piece in chunk(&text) {
                prop_assert!(
                    piece.chars().count() <= TARGET_CHARS
                        || !piece.contains(char::is_whitespace),
                    "over budget with a cut available: {piece:?}"
                );
            }
        }

        #[test]
        fn chunking_loses_nothing_but_whitespace(text in "(?s)[a-z .!?\n]{0,4000}") {
            prop_assert_eq!(squashed(&chunk(&text).join(" ")), squashed(&text));
        }

        #[test]
        fn chunking_loses_nothing_but_whitespace_in_unicode(text in "(?s).{0,4000}") {
            prop_assert_eq!(squashed(&chunk(&text).join(" ")), squashed(&text));
        }
    }
}
