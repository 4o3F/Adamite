use std::fmt::{self, Write};

/// Displays text with control characters escaped for terminal output.
#[derive(Debug, Clone, Copy)]
pub struct Escaped<'a>(
    /// Text whose control characters are escaped as `\u{…}`.
    pub &'a str,
);

impl fmt::Display for Escaped<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for character in self.0.chars() {
            if character.is_control() {
                write!(f, "{}", character.escape_unicode())?;
            } else {
                f.write_char(character)?;
            }
        }
        Ok(())
    }
}

/// Displays an escaped extension, using `(none)` for files without an extension.
#[derive(Debug, Clone, Copy)]
pub struct ExtensionLabel<'a>(
    /// Extension text, without the separating dot.
    pub &'a str,
);

impl fmt::Display for ExtensionLabel<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Escaped(if self.0.is_empty() { "(none)" } else { self.0 }).fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::{Escaped, ExtensionLabel};

    #[test]
    fn extension_labels_escape_untrusted_text_and_name_empty_extensions() {
        for (extension, expected) in [
            ("", "(none)"),
            ("txt", "txt"),
            ("\n\u{1b}", "\\u{a}\\u{1b}"),
        ] {
            assert_eq!(ExtensionLabel(extension).to_string(), expected);
        }
    }

    #[test]
    fn escapes_controls_and_preserves_printable_text() {
        for (input, expected) in [
            ("", ""),
            ("plain text", "plain text"),
            ("目录 café 🐈", "目录 café 🐈"),
            (
                "a\n\t\0\u{1b}\u{7f}\u{85}b",
                "a\\u{a}\\u{9}\\u{0}\\u{1b}\\u{7f}\\u{85}b",
            ),
        ] {
            assert_eq!(Escaped(input).to_string(), expected, "input {input:?}");
        }
    }
}
