//! Sanitising untrusted text (tag values, file names) before it reaches the terminal.
//!
//! Tag values and file names are attacker-controllable: a crafted file could embed
//! ANSI escape sequences that rewrite the terminal, or bidirectional overrides that
//! make one string look like another. Everything shown to the user passes through
//! [`sanitize`] first.

use std::path::Path;

/// Longest string kept from a single tag value or file name, in characters.
pub const MAX_TEXT_CHARS: usize = 256;

/// Returns `true` for invisible formatting characters (Unicode category Cf and
/// friends) that can reorder text or hide content: bidi embeddings/overrides/
/// isolates/marks, zero-width characters, soft hyphen, word joiner and invisible
/// operators, BOM, interlinear annotations and tag characters.
///
/// ZWNJ (U+200C) and the Mongolian variation selectors are kept: Persian, Indic
/// and Mongolian text needs them for correct shaping.
const fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'..='\u{1160}'
            | '\u{17B4}'..='\u{17B5}'
            | '\u{200B}'
            | '\u{200D}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
}

/// Cleans untrusted text for display.
///
/// Control characters (C0, DEL and C1 — including ESC and 8-bit CSI, so no
/// terminal escape sequence survives) become spaces, invisible formatting
/// characters are removed, runs of whitespace collapse to one space,
/// the result is trimmed and truncated to [`MAX_TEXT_CHARS`] characters.
/// Returns `None` when nothing printable is left.
#[must_use]
pub fn sanitize(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len().min(MAX_TEXT_CHARS * 4));
    let mut kept = 0usize;
    let mut pending_space = false;

    for c in raw.chars() {
        if is_invisible_format(c) {
            continue;
        }
        if c.is_control() || c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            if kept + 1 >= MAX_TEXT_CHARS {
                break;
            }
            out.push(' ');
            kept += 1;
            pending_space = false;
        }
        if kept >= MAX_TEXT_CHARS {
            break;
        }
        out.push(c);
        kept += 1;
    }

    (!out.is_empty()).then_some(out)
}

/// A path rendered for display: lossily decoded and sanitised.
#[must_use]
pub fn display_path(path: &Path) -> String {
    // Paths are not truncated: the user needs the full location in messages.
    path.to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_control() || is_invisible_format(c) {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_escape_sequences_and_controls() {
        assert_eq!(
            sanitize("\u{1b}[2J\u{1b}]0;pwned\u{7}Song\r\n").as_deref(),
            Some("[2J ]0;pwned Song")
        );
    }

    #[test]
    fn strips_bidi_overrides() {
        assert_eq!(sanitize("abc\u{202E}fed").as_deref(), Some("abcfed"));
        assert_eq!(sanitize("a\u{2066}b\u{2069}c").as_deref(), Some("abc"));
    }

    #[test]
    fn strips_c1_csi() {
        assert_eq!(sanitize("a\u{9B}31mb").as_deref(), Some("a 31mb"));
    }

    #[test]
    fn strips_invisible_format_characters() {
        let raw = "a\u{200B}b\u{200D}c\u{FEFF}d\u{00AD}e\u{2060}f\u{E0041}g\u{FFF9}h";
        assert_eq!(sanitize(raw).as_deref(), Some("abcdefgh"));
        assert_eq!(sanitize("\u{200B}\u{FEFF}"), None);
        // ZWNJ is meaningful in Persian/Indic text and is kept.
        assert_eq!(
            sanitize("می\u{200C}خواهم").as_deref(),
            Some("می\u{200C}خواهم")
        );
    }

    #[test]
    fn collapses_and_trims_whitespace() {
        assert_eq!(sanitize("  a \t\t b  ").as_deref(), Some("a b"));
    }

    #[test]
    fn empty_or_blank_is_none() {
        assert_eq!(sanitize(""), None);
        assert_eq!(sanitize(" \u{1b}\t\u{202E} "), None);
    }

    #[test]
    fn truncates_long_text() {
        let long = "é".repeat(MAX_TEXT_CHARS * 2);
        let cleaned = sanitize(&long).unwrap_or_default();
        assert_eq!(cleaned.chars().count(), MAX_TEXT_CHARS);
    }

    #[test]
    fn truncation_never_leaves_trailing_space() {
        let text = format!("{} b", "a".repeat(MAX_TEXT_CHARS - 1));
        let cleaned = sanitize(&text).unwrap_or_default();
        assert!(!cleaned.ends_with(' '));
        assert!(cleaned.chars().count() <= MAX_TEXT_CHARS);
    }

    #[test]
    fn display_path_replaces_controls() {
        assert_eq!(
            display_path(Path::new("/a\u{1b}[31m/b\u{202E}c")),
            "/a\u{FFFD}[31m/b\u{FFFD}c"
        );
    }
}
