//! Playlist names: they become file names, so they are validated strictly.

use std::fmt;

use crate::text::{is_invisible_format, sanitize};

/// Longest name, in characters.
pub const MAX_NAME_CHARS: usize = 100;

/// Longest name, in UTF-8 bytes, so that the temporary file name
/// `.<name>.m3u8.tmp-<pid>` fits the common 255-byte file name limit.
pub const MAX_NAME_BYTES: usize = 200;

/// Characters that are separators or reserved on some filesystem.
const FORBIDDEN: [char; 9] = ['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// Device names Windows reserves whatever the extension.
const RESERVED: [&str; 30] = [
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6",
    "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
];

/// Why a name was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    Empty,
    TooLong,
    Forbidden(char),
    /// Control characters, invisible formatting characters and whitespace
    /// other than a plain space, which could make one name look like another.
    Invisible,
    LeadingDot,
    Reserved,
    /// Another playlist has this name, ignoring case.
    Taken,
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("the name is empty"),
            Self::TooLong => write!(
                f,
                "the name is too long (at most {MAX_NAME_CHARS} characters and {MAX_NAME_BYTES} bytes)"
            ),
            Self::Forbidden(c) => write!(f, "the name cannot contain '{c}'"),
            Self::Invisible => f.write_str(
                "the name cannot contain control, invisible or special space characters",
            ),
            Self::LeadingDot => f.write_str("the name cannot start with '.'"),
            Self::Reserved => f.write_str("the name is reserved on Windows"),
            Self::Taken => f.write_str("the name is already used"),
        }
    }
}

impl std::error::Error for NameError {}

/// Checks a name typed by the user and returns it without surrounding spaces.
///
/// # Errors
///
/// Returns the first rule the name breaks.
pub fn validate_name(raw: &str) -> Result<&str, NameError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.chars().count() > MAX_NAME_CHARS || name.len() > MAX_NAME_BYTES {
        return Err(NameError::TooLong);
    }
    if let Some(c) = name.chars().find(|c| FORBIDDEN.contains(c)) {
        return Err(NameError::Forbidden(c));
    }
    if name
        .chars()
        .any(|c| c.is_control() || is_invisible_format(c) || (c.is_whitespace() && c != ' '))
    {
        return Err(NameError::Invisible);
    }
    if name.starts_with('.') {
        return Err(NameError::LeadingDot);
    }
    if is_reserved(name) {
        return Err(NameError::Reserved);
    }
    Ok(name)
}

/// [`validate_name`] plus uniqueness among `existing` names, ignoring case.
/// `current` is the name being renamed: changing only its case is allowed.
///
/// # Errors
///
/// Returns the first rule the name breaks, or [`NameError::Taken`].
pub fn check_new_name<'a, 'b>(
    raw: &'a str,
    existing: impl IntoIterator<Item = &'b str>,
    current: Option<&str>,
) -> Result<&'a str, NameError> {
    let name = validate_name(raw)?;
    let folded = fold(name);
    let taken = existing
        .into_iter()
        .filter(|other| Some(*other) != current)
        .any(|other| fold(other) == folded);
    if taken {
        Err(NameError::Taken)
    } else {
        Ok(name)
    }
}

/// The form names are compared in: as displayed (so names made outside kasset
/// that only differ in invisible or special space characters count as equal),
/// ignoring case.
fn fold(name: &str) -> String {
    sanitize(name).unwrap_or_default().to_lowercase()
}

/// Windows treats `CON`, `con.txt`, `CON .x` and so on as the device.
fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_names_and_trims() {
        assert_eq!(validate_name("  Road trip "), Ok("Road trip"));
        assert_eq!(validate_name("Café ☕ 2024"), Ok("Café ☕ 2024"));
        assert_eq!(validate_name("Console"), Ok("Console"));
        assert_eq!(validate_name("a.b"), Ok("a.b"));
    }

    #[test]
    fn refuses_empty() {
        assert_eq!(validate_name(""), Err(NameError::Empty));
        assert_eq!(validate_name("   "), Err(NameError::Empty));
    }

    #[test]
    fn refuses_long_names_by_chars_and_bytes() {
        assert_eq!(
            validate_name(&"a".repeat(MAX_NAME_CHARS)).map(str::len),
            Ok(100)
        );
        assert_eq!(
            validate_name(&"a".repeat(MAX_NAME_CHARS + 1)),
            Err(NameError::TooLong)
        );
        // 70 Japanese characters: within 100 characters but 210 bytes.
        let japanese = "曲".repeat(70);
        assert_eq!(japanese.len(), 210);
        let err = validate_name(&japanese);
        assert_eq!(err, Err(NameError::TooLong));
        assert!(NameError::TooLong.to_string().contains("too long"));
    }

    #[test]
    fn refuses_forbidden_characters() {
        for c in FORBIDDEN {
            assert_eq!(
                validate_name(&format!("a{c}b")),
                Err(NameError::Forbidden(c))
            );
        }
        assert!(NameError::Forbidden('/').to_string().contains('/'));
    }

    #[test]
    fn refuses_control_and_invisible_characters() {
        for name in [
            "a\u{1b}[31m",
            "a\tb",
            "a\u{9b}b",
            "a\u{202E}b",
            "a\u{A0}b",
            "a\u{2028}b",
            "a\u{3000}b",
            "a\u{200B}b",
        ] {
            assert_eq!(validate_name(name), Err(NameError::Invisible), "{name:?}");
        }
    }

    #[test]
    fn refuses_leading_dot() {
        assert_eq!(validate_name(".hidden"), Err(NameError::LeadingDot));
        assert_eq!(validate_name(" ..x"), Err(NameError::LeadingDot));
    }

    #[test]
    fn refuses_windows_reserved_names() {
        for name in [
            "CON", "con", "Nul", "com1", "LPT9", "aux.mix", "PRN .x", "COM¹",
        ] {
            assert_eq!(validate_name(name), Err(NameError::Reserved), "{name}");
        }
        assert_eq!(validate_name("CONCERT"), Ok("CONCERT"));
        assert_eq!(validate_name("COM10"), Ok("COM10"));
    }

    #[test]
    fn uniqueness_ignores_case() {
        let existing = ["Running", "Gym"];
        assert_eq!(
            check_new_name("running", existing, None),
            Err(NameError::Taken)
        );
        assert_eq!(
            check_new_name(" GYM ", existing, None),
            Err(NameError::Taken)
        );
        assert_eq!(check_new_name("Road trip", existing, None), Ok("Road trip"));
    }

    #[test]
    fn uniqueness_compares_displayed_form() {
        // Made outside kasset: shown as "a b" and "xy".
        let existing = ["a\u{A0}b", "x\u{200B}y"];
        assert_eq!(check_new_name("A B", existing, None), Err(NameError::Taken));
        assert_eq!(check_new_name("xy", existing, None), Err(NameError::Taken));
    }

    #[test]
    fn rename_may_change_only_case() {
        let existing = ["Running", "Gym"];
        assert_eq!(
            check_new_name("RUNNING", existing, Some("Running")),
            Ok("RUNNING")
        );
        assert_eq!(
            check_new_name("gym", existing, Some("Running")),
            Err(NameError::Taken)
        );
        assert_eq!(
            check_new_name("a/b", existing, Some("Running")),
            Err(NameError::Forbidden('/'))
        );
    }
}
