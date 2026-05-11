use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Read a file as UTF-8, falling back to a best-effort decode for non-UTF-8.
pub fn read_text<P: AsRef<Path>>(path: P) -> Result<String, IoError> {
    let bytes = std::fs::read(path)?;
    if let Ok(s) = std::str::from_utf8(&bytes) {
        return Ok(s.to_string());
    }
    let (cow, _, _) = encoding_rs::UTF_8.decode(&bytes);
    Ok(cow.into_owned())
}

pub fn write_text<P: AsRef<Path>>(path: P, content: &str) -> Result<(), IoError> {
    std::fs::write(path, content)?;
    Ok(())
}
