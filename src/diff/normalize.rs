use super::Whitespace;

/// Normalize a line for whitespace-insensitive comparison.
/// The returned `String` is what the engine sees; the original line text
/// is preserved separately by the caller and used in emitted `DiffOp`s.
pub fn normalize_line(line: &str, mode: Whitespace) -> String {
    match mode {
        Whitespace::None => line.to_string(),
        Whitespace::IgnoreAll => line.chars().filter(|c| !c.is_whitespace()).collect(),
        Whitespace::IgnoreLeading => line.trim_start().to_string(),
        Whitespace::IgnoreTrailingEol => {
            // trim_end handles trailing spaces; strip a final '\r' to fold CRLF == LF.
            let trimmed = line.trim_end();
            trimmed.strip_suffix('\r').unwrap_or(trimmed).to_string()
        }
    }
}

/// Normalize an entire line slice.
pub fn normalize_lines(lines: &[&str], mode: Whitespace) -> Vec<String> {
    lines.iter().map(|l| normalize_line(l, mode)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignore_all() {
        assert_eq!(normalize_line("  a  b\t c  ", Whitespace::IgnoreAll), "abc");
    }

    #[test]
    fn ignore_leading() {
        assert_eq!(normalize_line("\t  hi  ", Whitespace::IgnoreLeading), "hi  ");
    }

    #[test]
    fn ignore_trailing_eol() {
        assert_eq!(normalize_line("hi  \r", Whitespace::IgnoreTrailingEol), "hi");
        assert_eq!(normalize_line("hi", Whitespace::IgnoreTrailingEol), "hi");
    }

    #[test]
    fn none() {
        assert_eq!(normalize_line("  hi  ", Whitespace::None), "  hi  ");
    }
}
