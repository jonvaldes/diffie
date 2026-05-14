use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Result of reading a text file: the contents WITHOUT any trailing
/// `\n`, plus a flag indicating whether the source file ended in `\n`.
/// `write_text` re-applies the trailing newline based on the flag so
/// save/load preserves the original convention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextRead {
    pub text: String,
    pub trailing_newline: bool,
}

pub fn read_text<P: AsRef<Path>>(path: P) -> Result<TextRead, IoError> {
    let bytes = std::fs::read(path)?;
    let raw = if let Ok(s) = std::str::from_utf8(&bytes) {
        s.to_string()
    } else {
        let (cow, _, _) = encoding_rs::UTF_8.decode(&bytes);
        cow.into_owned()
    };
    let trailing_newline = raw.ends_with('\n');
    let text = if trailing_newline {
        raw[..raw.len() - 1].to_string()
    } else {
        raw
    };
    Ok(TextRead { text, trailing_newline })
}

pub fn write_text<P: AsRef<Path>>(
    path: P,
    content: &str,
    trailing_newline: bool,
) -> Result<(), IoError> {
    if trailing_newline {
        let mut out = String::with_capacity(content.len() + 1);
        out.push_str(content);
        out.push('\n');
        std::fs::write(path, out)?;
    } else {
        std::fs::write(path, content)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_strips_trailing_newline_and_records_flag() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "alpha\nbeta\n").unwrap();
        let r = read_text(&p).unwrap();
        assert_eq!(r.text, "alpha\nbeta");
        assert!(r.trailing_newline);
    }

    #[test]
    fn read_keeps_text_when_no_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "alpha\nbeta").unwrap();
        let r = read_text(&p).unwrap();
        assert_eq!(r.text, "alpha\nbeta");
        assert!(!r.trailing_newline);
    }

    #[test]
    fn write_preserves_trailing_newline_via_flag() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        write_text(&p, "alpha\nbeta", true).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"alpha\nbeta\n");
        write_text(&p, "alpha\nbeta", false).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"alpha\nbeta");
    }
}
