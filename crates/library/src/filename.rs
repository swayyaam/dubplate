//! Guessing artist and title from a filename.
//!
//! Twenty-three percent of a real DJ collection arrives with no artist tag at
//! all -- eighty-seven percent of the WAVs -- because pools and rips hand you
//! `Artist - Title (Remix).wav` and nothing else. The name is the only metadata
//! those files have, and it is usually right.
//!
//! Every rule here earns its place by appearing in a real library. Nothing
//! guesses cleverly: a filename that does not match a pattern produces a title
//! and no artist, which is exactly what it knows.

use serde::Serialize;

/// Bracketed asides that describe the file rather than the music.
///
/// Only removed when the whole bracket matches: `(Extended Mix)` and
/// `(Tasty Or Not Remix)` are part of the title and must survive, so this
/// cannot be "strip anything in brackets".
const JUNK_PHRASES: [&str; 18] = [
    "official video",
    "official audio",
    "official music video",
    "official lyric video",
    "lyric video",
    "lyrics",
    "visualiser",
    "visualizer",
    "audio",
    "video",
    "hq",
    "hd",
    "free download",
    "free dl",
    "download",
    "out now",
    "preview",
    "master",
];

/// What a filename appears to say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Guess {
    pub title: Option<String>,
    pub artist: Option<String>,
    /// A leading track number, as album rips are almost always named.
    pub track: Option<u32>,
    /// BPM written into the name, e.g. `... 71 BPM C# Min`. Reported so a
    /// preview can show it; never written as a tag, because a number in a
    /// filename is a claim and the analyser makes a measurement.
    pub bpm: Option<u32>,
    /// Musical key written into the name, in whatever form it was found.
    pub key: Option<String>,
}

/// Read a filename, with or without its extension.
pub fn parse(file_name: &str) -> Guess {
    let stem = strip_extension(file_name);
    let (stem, track) = take_track_number(&stem);
    let (stem, bpm, key) = take_tempo_and_key(&stem);
    let cleaned = strip_junk(&stem);

    let (artist, title) = split_artist_title(&cleaned);
    Guess {
        title: normalise(title),
        artist: artist.and_then(normalise),
        track,
        bpm,
        key,
    }
}

/// Pull a leading track number off the name.
///
/// A delimiter is required -- `12. Title` and `03 - Title`, never `6 Million
/// ID`. Without that, any title that happens to start with a number loses it,
/// and titles starting with numbers are common.
fn take_track_number(stem: &str) -> (String, Option<u32>) {
    let trimmed = stem.trim_start();
    let digits: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 3 {
        return (stem.to_owned(), None);
    }
    let rest = trimmed[digits.len()..].trim_start();
    let Some(delimiter) = rest.chars().next() else {
        return (stem.to_owned(), None);
    };
    if !matches!(delimiter, '.' | '-' | '_') {
        return (stem.to_owned(), None);
    }
    let remainder = rest[delimiter.len_utf8()..].trim_start();
    // A number and a delimiter with nothing after them is not a track number,
    // it is the whole name.
    if remainder.is_empty() {
        return (stem.to_owned(), None);
    }
    match digits.parse::<u32>() {
        Ok(number) if number > 0 => (remainder.to_owned(), Some(number)),
        _ => (stem.to_owned(), None),
    }
}

fn strip_extension(name: &str) -> String {
    match name.rsplit_once('.') {
        // Only treat it as an extension if it looks like one. A title ending in
        // ". 2" is not a file type.
        Some((stem, ext)) if (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) => {
            stem.to_owned()
        }
        _ => name.to_owned(),
    }
}

/// Pull a trailing `123 BPM` and a musical key out of the name.
///
/// Producers label files this way and the label is worth surfacing, but it goes
/// in the preview rather than into a tag: the analysis pass measures both, and
/// a measurement beats a filename.
fn take_tempo_and_key(stem: &str) -> (String, Option<u32>, Option<String>) {
    let mut bpm = None;
    let mut key = None;
    let mut kept: Vec<&str> = Vec::new();

    let words: Vec<&str> = stem.split_whitespace().collect();
    let mut index = 0;
    while index < words.len() {
        let word = words[index];
        // "71 BPM"
        if index + 1 < words.len() && words[index + 1].trim_matches(is_bracket).eq_ignore_ascii_case("bpm") {
            if let Ok(value) = word.trim_matches(is_bracket).parse::<u32>() {
                if (40..=250).contains(&value) {
                    bpm = Some(value);
                    index += 2;
                    continue;
                }
            }
        }
        // "C# Min", "F Maj", "Am"
        if let Some(found) = read_key(&words[index..]) {
            key = Some(found.0);
            index += found.1;
            continue;
        }
        kept.push(word);
        index += 1;
    }

    (kept.join(" "), bpm, key)
}

/// A musical key at the start of `words`, and how many words it spans.
fn read_key(words: &[&str]) -> Option<(String, usize)> {
    let root = words.first()?.trim_matches(is_bracket);
    let mut chars = root.chars();
    let letter = chars.next()?;
    if !('A'..='G').contains(&letter.to_ascii_uppercase()) {
        return None;
    }
    let accidental: String = chars.clone().take_while(|c| *c == '#' || *c == 'b').collect();
    let rest: String = chars.skip(accidental.len()).collect();

    // "Am" / "Amin" -- quality attached to the root.
    if !rest.is_empty() {
        let quality = rest.to_ascii_lowercase();
        if ["m", "min", "minor", "maj", "major"].contains(&quality.as_str()) {
            return Some((format!("{letter}{accidental}{rest}"), 1));
        }
        return None;
    }
    // "A Min" -- quality as the next word.
    let next = words.get(1)?.trim_matches(is_bracket).to_ascii_lowercase();
    if ["m", "min", "minor", "maj", "major"].contains(&next.as_str()) {
        return Some((format!("{letter}{accidental} {next}"), 2));
    }
    None
}

fn is_bracket(c: char) -> bool {
    matches!(c, '(' | ')' | '[' | ']' | '{' | '}')
}

/// Remove bracketed asides that describe the file rather than the music, plus a
/// bare trailing bitrate.
fn strip_junk(stem: &str) -> String {
    let mut out = String::with_capacity(stem.len());
    let mut rest = stem;

    while let Some(open) = rest.find(['(', '[']) {
        let closer = if rest.as_bytes()[open] == b'(' { ')' } else { ']' };
        let Some(close) = rest[open..].find(closer).map(|i| open + i) else {
            break;
        };
        let inner = &rest[open + 1..close];
        out.push_str(&rest[..open]);
        if !is_junk(inner) {
            out.push_str(&rest[open..=close]);
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// True for a bracketed phrase that says nothing about the music.
fn is_junk(inner: &str) -> bool {
    let lowered = inner.trim().to_ascii_lowercase();
    if lowered.is_empty() {
        return true;
    }
    // "320kbps", "128 kbps", "24bit", "44.1khz"
    if lowered.ends_with("kbps") || lowered.ends_with("kbit") || lowered.ends_with("khz") || lowered.ends_with("bit") {
        let head = lowered
            .trim_end_matches(|c: char| c.is_ascii_alphabetic())
            .trim();
        if !head.is_empty() && head.chars().all(|c| c.is_ascii_digit() || c == '.' || c == ' ') {
            return true;
        }
    }
    JUNK_PHRASES.contains(&lowered.as_str())
}

/// Split on the first " - ", which is the near-universal convention.
///
/// The *first* separator, so `NUCLEYA - Jogan - Nucleya X Gulshan Kumar` gives
/// the artist NUCLEYA and keeps the rest as the title. A name with no separator
/// is all title, because inventing an artist from nothing is worse than leaving
/// the field empty.
fn split_artist_title(cleaned: &str) -> (Option<&str>, &str) {
    match cleaned.split_once(" - ") {
        Some((artist, title)) if !artist.trim().is_empty() && !title.trim().is_empty() => {
            (Some(artist), title)
        }
        _ => (None, cleaned),
    }
}

fn normalise(value: &str) -> Option<String> {
    let mut cleaned = value.trim().replace('_', " ");
    while cleaned.contains("  ") {
        cleaned = cleaned.replace("  ", " ");
    }
    let cleaned = cleaned.trim_matches(|c: char| c == '-' || c.is_whitespace()).to_owned();
    (!cleaned.is_empty()).then_some(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guess(name: &str) -> Guess {
        parse(name)
    }

    #[test]
    fn the_common_case_is_artist_dash_title() {
        let g = guess("Aditya Rikhari - Kol Aa.wav");
        assert_eq!(g.artist.as_deref(), Some("Aditya Rikhari"));
        assert_eq!(g.title.as_deref(), Some("Kol Aa"));
    }

    #[test]
    fn a_remix_in_brackets_is_part_of_the_title() {
        // The whole reason junk removal cannot just strip brackets.
        let g = guess("Saint Jhn x Natty Rico - Roses Madinina (Natty Rico Remix).wav");
        assert_eq!(g.artist.as_deref(), Some("Saint Jhn x Natty Rico"));
        assert_eq!(g.title.as_deref(), Some("Roses Madinina (Natty Rico Remix)"));
    }

    #[test]
    fn only_the_first_separator_splits() {
        let g = guess("NUCLEYA - Jogan - Nucleya X Gulshan Kumar.wav");
        assert_eq!(g.artist.as_deref(), Some("NUCLEYA"));
        assert_eq!(g.title.as_deref(), Some("Jogan - Nucleya X Gulshan Kumar"));
    }

    #[test]
    fn a_name_with_no_separator_is_all_title() {
        let g = guess("Kar Gayi Chull.mp3");
        assert_eq!(g.artist, None);
        assert_eq!(g.title.as_deref(), Some("Kar Gayi Chull"));
    }

    #[test]
    fn descriptions_of_the_file_are_removed_but_not_the_music() {
        let g = guess("Fred again.. - Kammy (like i do) [Visualiser] (320kbps).mp3");
        assert_eq!(g.artist.as_deref(), Some("Fred again.."));
        // "(like i do)" is part of the title; the other two are not.
        assert_eq!(g.title.as_deref(), Some("Kammy (like i do)"));
    }

    #[test]
    fn a_tempo_and_key_in_the_name_are_reported_not_written() {
        let g = guess("Monsoon (Trap) 71 BPM C# Min.mp3");
        assert_eq!(g.bpm, Some(71));
        assert_eq!(g.key.as_deref(), Some("C# min"));
        // "(Trap)" is a genre marker inside the title and is left alone.
        assert_eq!(g.title.as_deref(), Some("Monsoon (Trap)"));
        assert_eq!(g.artist, None);
    }

    #[test]
    fn an_album_rip_gives_up_its_track_number() {
        let g = guess("12. Every Angel is Terrifying.flac");
        assert_eq!(g.track, Some(12));
        assert_eq!(g.title.as_deref(), Some("Every Angel is Terrifying"));
        assert_eq!(g.artist, None);

        let g = guess("03 - Push My Luck (Ares Carter Remix).flac");
        assert_eq!(g.track, Some(3));
        assert_eq!(g.title.as_deref(), Some("Push My Luck (Ares Carter Remix)"));
    }

    #[test]
    fn a_title_that_starts_with_a_number_keeps_it() {
        // The reason a delimiter is required. "6 Million ID" is not track six.
        let g = guess("Skrillex - 6 Million ID.mp3");
        assert_eq!(g.track, None);
        assert_eq!(g.title.as_deref(), Some("6 Million ID"));

        let g = guess("1979.flac");
        assert_eq!(g.track, None);
        assert_eq!(g.title.as_deref(), Some("1979"));
    }

    #[test]
    fn a_track_number_before_an_artist_still_splits() {
        let g = guess("01 - Fred again.. - Danielle.flac");
        assert_eq!(g.track, Some(1));
        assert_eq!(g.artist.as_deref(), Some("Fred again.."));
        assert_eq!(g.title.as_deref(), Some("Danielle"));
    }

    #[test]
    fn a_note_letter_in_a_title_is_not_mistaken_for_a_key() {
        // "B" is a note name but "Be" is not a key, and neither is a bare word.
        let g = guess("Let It Be.flac");
        assert_eq!(g.key, None);
        assert_eq!(g.title.as_deref(), Some("Let It Be"));
    }

    #[test]
    fn an_absurd_tempo_is_not_taken_as_one() {
        let g = guess("Studio 4000 BPM Session.wav");
        assert_eq!(g.bpm, None, "4000 is not a tempo");
        assert!(g.title.as_deref().unwrap().contains("4000"));
    }

    #[test]
    fn underscores_and_double_spaces_are_tidied() {
        let g = guess("Some_Artist  -  Some_Title.flac");
        assert_eq!(g.artist.as_deref(), Some("Some Artist"));
        assert_eq!(g.title.as_deref(), Some("Some Title"));
    }

    #[test]
    fn a_dot_in_a_title_is_not_an_extension() {
        let g = guess("Fred again.. - Danielle");
        assert_eq!(g.artist.as_deref(), Some("Fred again.."));
        assert_eq!(g.title.as_deref(), Some("Danielle"));
    }

    #[test]
    fn an_empty_or_junk_only_name_produces_nothing_rather_than_rubbish() {
        assert_eq!(parse("").title, None);
        assert_eq!(parse("(Official Video).mp4").title, None);
        assert_eq!(parse("   .flac").title, None);
    }

    #[test]
    fn a_leading_dash_does_not_become_an_empty_artist() {
        let g = guess("- Untitled.wav");
        assert_eq!(g.artist, None);
        assert_eq!(g.title.as_deref(), Some("Untitled"));
    }
}
