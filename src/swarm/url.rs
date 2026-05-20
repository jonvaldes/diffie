//! Parse Helix Swarm URLs of the form `<scheme>://<authority>/reviews/<id>`
//! and `<scheme>://<authority>/changes/<id>`. Trailing path segments
//! (e.g. `/files`), trailing slashes, query strings, and fragments are
//! tolerated and stripped.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwarmTarget {
    Review(u64),
    Change(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwarmUrl {
    /// `scheme://authority` (no trailing slash). Used both for HTTP requests
    /// and as the per-host keychain key.
    pub host: String,
    pub target: SwarmTarget,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("not a URL")]
    NotUrl,
    #[error("expected /reviews/<id> or /changes/<id>")]
    BadPath,
    #[error("review/change id is not a positive integer")]
    BadId,
    #[error("only http/https schemes are supported")]
    BadScheme,
}

pub fn parse(s: &str) -> Result<SwarmUrl, ParseError> {
    let u = url::Url::parse(s.trim()).map_err(|_| ParseError::NotUrl)?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(ParseError::BadScheme);
    }
    let host = u.host_str().ok_or(ParseError::NotUrl)?;
    let port = u.port().map(|p| format!(":{p}")).unwrap_or_default();
    let host = format!("{}://{}{}", u.scheme(), host, port);
    let segments: Vec<&str> = u
        .path_segments()
        .ok_or(ParseError::BadPath)?
        .filter(|s| !s.is_empty())
        .collect();
    if segments.len() < 2 {
        return Err(ParseError::BadPath);
    }
    if !matches!(segments[0], "reviews" | "changes") {
        return Err(ParseError::BadPath);
    }
    let id: u64 = segments[1].parse().map_err(|_| ParseError::BadId)?;
    if id == 0 {
        return Err(ParseError::BadId);
    }
    let target = match segments[0] {
        "reviews" => SwarmTarget::Review(id),
        "changes" => SwarmTarget::Change(id),
        _ => unreachable!(),
    };
    Ok(SwarmUrl { host, target })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_review_url() {
        let u = parse("https://swarm.example.com/reviews/12345").unwrap();
        assert_eq!(u.host, "https://swarm.example.com");
        assert_eq!(u.target, SwarmTarget::Review(12345));
    }

    #[test]
    fn parses_change_url() {
        let u = parse("https://swarm.example.com/changes/777").unwrap();
        assert_eq!(u.target, SwarmTarget::Change(777));
    }

    #[test]
    fn tolerates_trailing_files_segment() {
        let u = parse("https://s/reviews/1/files").unwrap();
        assert_eq!(u.target, SwarmTarget::Review(1));
    }

    #[test]
    fn tolerates_trailing_slash_query_fragment() {
        let u = parse("https://s/reviews/1/?foo=bar#x").unwrap();
        assert_eq!(u.target, SwarmTarget::Review(1));
    }

    #[test]
    fn preserves_port_in_host() {
        let u = parse("http://swarm.local:8080/changes/9").unwrap();
        assert_eq!(u.host, "http://swarm.local:8080");
    }

    #[test]
    fn rejects_non_swarm_path() {
        assert_eq!(parse("https://s/users/bob"), Err(ParseError::BadPath));
    }

    #[test]
    fn rejects_zero_id() {
        assert_eq!(parse("https://s/reviews/0"), Err(ParseError::BadId));
    }

    #[test]
    fn rejects_non_numeric_id() {
        assert_eq!(parse("https://s/reviews/abc"), Err(ParseError::BadId));
    }

    #[test]
    fn rejects_non_http_scheme() {
        assert_eq!(parse("ftp://s/reviews/1"), Err(ParseError::BadScheme));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse("not a url"), Err(ParseError::NotUrl));
    }
}
