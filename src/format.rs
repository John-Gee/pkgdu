use std::fmt;

/// Format string token — what `%n`, `%m`, etc. expand to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FormatToken {
    Name,         // %n
    Version,      // %v
    RealSize,     // %m
    ApparentSize, // %a
    FileCount,    // %f
    MetaSize,     // %p
    BtrfsSize,    // %z
    BtrfsRatio,   // %r
    Percent,      // %%
}

impl fmt::Display for FormatToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FormatToken::Name => write!(f, "name"),
            FormatToken::Version => write!(f, "version"),
            FormatToken::RealSize => write!(f, "real_size"),
            FormatToken::ApparentSize => write!(f, "apparent_size"),
            FormatToken::FileCount => write!(f, "file_count"),
            FormatToken::MetaSize => write!(f, "meta_size"),
            FormatToken::BtrfsSize => write!(f, "btrfs_size"),
            FormatToken::BtrfsRatio => write!(f, "btrfs_ratio"),
            FormatToken::Percent => write!(f, "percent"),
        }
    }
}

/// A single segment of a parsed format string.
#[derive(Debug, Clone)]
pub enum FormatSegment {
    Literal(String),
    Token(FormatToken),
}

/// A parsed format string — a sequence of literal and token segments.
#[derive(Debug, Clone)]
pub struct FormatString(pub Vec<FormatSegment>);

impl FormatString {
    /// Render the format string for a given package result into an output line.
    pub fn render(&self, pkg: &crate::scan::PackageResult, cfg: &crate::Config) -> String {
        use crate::human_size::{format_size, UnitSpec};

        let mut result = String::new();
        for segment in &self.0 {
            match segment {
                FormatSegment::Literal(s) => result.push_str(s),
                FormatSegment::Token(tok) => match tok {
                    FormatToken::Name => result.push_str(&pkg.name),
                    FormatToken::Version => result.push_str(&pkg.version),
                    FormatToken::RealSize => {
                        result.push_str(&format_size(pkg.real_size, UnitSpec::Auto))
                    }
                    FormatToken::ApparentSize => {
                        result.push_str(&format_size(pkg.apparent_size, UnitSpec::Auto))
                    }
                    FormatToken::FileCount => result.push_str(&pkg.file_count.to_string()),
                    FormatToken::MetaSize => {
                        result.push_str(&format_size(pkg.metadata_size, UnitSpec::Auto))
                    }
                    FormatToken::BtrfsSize => {
                        if cfg.btrfs {
                            match pkg.btrfs_compressed {
                                Some(s) => result.push_str(&format_size(s, UnitSpec::Auto)),
                                None => result.push_str("N/A"),
                            }
                        } else {
                            result.push_str("N/A");
                        }
                    }
                    FormatToken::BtrfsRatio => {
                        if cfg.btrfs {
                            match pkg.btrfs_compressed {
                                Some(s) if s > 0 => {
                                    let ratio = s as f64 / pkg.real_size as f64 * 100.0;
                                    result.push_str(&format!("{:.0}%", ratio));
                                }
                                _ => result.push_str("N/A"),
                            }
                        } else {
                            result.push_str("N/A");
                        }
                    }
                    FormatToken::Percent => result.push('%'),
                },
            }
        }
        result
    }

    /// Parse a raw format string into segments.
    pub fn parse(s: &str) -> Result<FormatString, String> {
        if s.contains('\0') {
            return Err("NUL byte in format".to_string());
        }
        let segments = parse_segments(s);
        Ok(FormatString(segments))
    }

    /// Detect if the first positional argument is a format string (contains `%`).
    #[allow(dead_code)]
    pub fn is_format_string(arg: &str) -> bool {
        arg.contains('%')
    }
}

fn parse_segments(input: &str) -> Vec<FormatSegment> {
    let mut segments = Vec::new();
    let mut current_literal = String::new();
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        if chars[i] == '\\' && i + 1 < len {
            if !current_literal.is_empty() {
                segments.push(FormatSegment::Literal(current_literal.clone()));
                current_literal.clear();
            }
            match chars[i + 1] {
                't' => current_literal.push('\t'),
                'n' => current_literal.push('\n'),
                '\\' => current_literal.push('\\'),
                _ => {
                    current_literal.push('\\');
                    current_literal.push(chars[i + 1]);
                }
            }
            i += 2;
        } else if chars[i] == '%' && i + 1 < len {
            if !current_literal.is_empty() {
                segments.push(FormatSegment::Literal(current_literal.clone()));
                current_literal.clear();
            }
            let token = match chars[i + 1] {
                'n' => Some(FormatToken::Name),
                'v' => Some(FormatToken::Version),
                'm' => Some(FormatToken::RealSize),
                'a' => Some(FormatToken::ApparentSize),
                'f' => Some(FormatToken::FileCount),
                'p' => Some(FormatToken::MetaSize),
                'z' => Some(FormatToken::BtrfsSize),
                'r' => Some(FormatToken::BtrfsRatio),
                '%' => Some(FormatToken::Percent),
                _ => None,
            };
            match token {
                Some(tok) => segments.push(FormatSegment::Token(tok)),
                None => {
                    current_literal.push('%');
                    if i + 1 < len {
                        current_literal.push(chars[i + 1]);
                        i += 1;
                    }
                }
            }
            i += 2;
        } else if chars[i] == '%' && i + 1 == len {
            current_literal.push('%');
            i += 1;
        } else if chars[i] == '\\' && i + 1 == len {
            current_literal.push('\\');
            i += 1;
        } else {
            current_literal.push(chars[i]);
            i += 1;
        }
    }

    if !current_literal.is_empty() {
        segments.push(FormatSegment::Literal(current_literal));
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_escape_sequences() {
        let fmt = FormatString::parse(r"hello\tworld\nend").unwrap();
        assert!(fmt
            .0
            .iter()
            .any(|s| matches!(s, FormatSegment::Literal(l) if l.contains('\t'))));
        assert!(fmt
            .0
            .iter()
            .any(|s| matches!(s, FormatSegment::Literal(l) if l.contains('\n'))));
    }

    #[test]
    fn test_format_literal_percent() {
        let fmt = FormatString::parse("100%% done").unwrap();
        assert_eq!(fmt.0.len(), 3);
        assert!(matches!(
            &fmt.0[1],
            FormatSegment::Token(FormatToken::Percent)
        ));
    }

    #[test]
    fn test_format_unknown_token() {
        let fmt = FormatString::parse("%x value").unwrap();
        assert!(fmt
            .0
            .iter()
            .any(|s| matches!(s, FormatSegment::Literal(l) if l.contains('%'))));
    }

    #[test]
    fn test_format_string_empty() {
        let fmt = FormatString::parse("").unwrap();
        assert!(fmt.0.is_empty());
    }

    #[test]
    fn test_format_nul_byte_fails() {
        let result = FormatString::parse("hello\x00world");
        assert!(result.is_err());
    }

    #[test]
    fn test_format_mixed_tokens_and_literals() {
        let fmt = FormatString::parse("%n: %v").unwrap();
        assert_eq!(fmt.0.len(), 3);
        assert!(matches!(&fmt.0[0], FormatSegment::Token(FormatToken::Name)));
        assert!(matches!(&fmt.0[1], FormatSegment::Literal(l) if l == ": "));
        assert!(matches!(
            &fmt.0[2],
            FormatSegment::Token(FormatToken::Version)
        ));
    }

    #[test]
    fn test_format_backslash_escape() {
        let fmt = FormatString::parse(r"path\to\file").unwrap();
        assert!(fmt
            .0
            .iter()
            .any(|s| matches!(s, FormatSegment::Literal(l) if l.contains('\t'))));
    }

    #[test]
    fn test_format_lone_trailing_percent() {
        let fmt = FormatString::parse("value%").unwrap();
        assert!(fmt
            .0
            .iter()
            .any(|s| matches!(s, FormatSegment::Literal(l) if l.contains('%'))));
    }

    #[test]
    fn test_format_lone_trailing_backslash() {
        let fmt = FormatString::parse("end\\").unwrap();
        assert!(fmt
            .0
            .iter()
            .any(|s| matches!(s, FormatSegment::Literal(l) if l.ends_with('\\'))));
    }

    #[test]
    fn test_format_all_known_tokens() {
        let input = "%n %v %m %a %f %p %z %r %%";
        let fmt = FormatString::parse(input).unwrap();
        let token_types: Vec<_> = fmt
            .0
            .iter()
            .filter_map(|s| {
                if matches!(s, FormatSegment::Token(_)) {
                    Some(())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(token_types.len(), 9);
    }
}
