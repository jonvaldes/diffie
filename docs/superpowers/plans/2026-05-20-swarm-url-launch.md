# Swarm URL Launch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Launch Diffie with a Helix Swarm review or changelist URL; open one read-only 2-way diff tab per file plus a read-only info tab.

**Architecture:** A new core `swarm` module (URL parsing, REST client, models, background loader) feeds a new `TabMode::SwarmInfo` and a `read_only` flag on `DiffSession`. Loader runs on a background thread and posts events through an mpsc channel drained each frame.

**Tech Stack:** Rust, `ureq` (blocking HTTP), `serde_json`, `keyring` (OS credential store), `url`, `open`, existing `imgui-rs` + `wgpu` + `winit` GUI.

**Spec:** `docs/superpowers/specs/2026-05-20-swarm-url-launch-design.md`

---

## File Structure

**New:**
- `src/swarm/mod.rs` — module root, re-exports
- `src/swarm/url.rs` — URL parsing (`SwarmTarget`, `SwarmUrl`, `parse`)
- `src/swarm/model.rs` — serde DTOs (`ReviewMeta`, `FileEntry`, `FileAction`, `SidePayload`)
- `src/swarm/client.rs` — `SwarmApi` trait + `Client` (ureq impl) + `Error`
- `src/swarm/loader.rs` — background orchestrator, `LoaderEvent`, `LoaderHandle`
- `src/app/swarm_creds.rs` — keychain helpers
- `src/app/swarm_login.rs` — login modal state machine + render
- `src/app/swarm_info_view.rs` — info tab render
- `tests/swarm_fixtures/` — JSON fixtures (review, change, files, error)

**Modified:**
- `Cargo.toml` — add deps under `gui` feature, plus `url` in core
- `src/lib.rs` — `pub mod swarm;`
- `src/main.rs` — single-arg Swarm URL dispatch
- `src/session.rs` — `read_only` field + guard in `set_side_text`
- `src/app/mod.rs` — `InitialOpen::Swarm`, `TabMode::SwarmInfo`, loader receiver field, frame drain, menu guards
- `src/app/diff_view/mod.rs` — read-only render path + per-side display overlay
- `src/app/undo_stack.rs` — skip stack creation when session is read-only

---

## Task 1: URL parser

**Files:**
- Create: `src/swarm/mod.rs`
- Create: `src/swarm/url.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Add `pub mod swarm;` to lib.rs**

In `src/lib.rs`, add a new line near the other `pub mod` declarations:

```rust
pub mod swarm;
```

- [ ] **Step 2: Create the module file**

Create `src/swarm/mod.rs`:

```rust
//! Helix Swarm integration: URL parsing, REST client, background loader.

pub mod url;
```

- [ ] **Step 3: Write failing tests for URL parser**

Create `src/swarm/url.rs`:

```rust
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
    let id: u64 = segments[1].parse().map_err(|_| ParseError::BadId)?;
    if id == 0 {
        return Err(ParseError::BadId);
    }
    let target = match segments[0] {
        "reviews" => SwarmTarget::Review(id),
        "changes" => SwarmTarget::Change(id),
        _ => return Err(ParseError::BadPath),
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
```

- [ ] **Step 4: Add `url` dep**

Edit `Cargo.toml`. Under `[dependencies]` (the core section, before `# gui`), add:

```toml
url = "2"
```

- [ ] **Step 5: Run tests; expect compile failures, then pass**

Run: `cargo test --no-default-features --lib swarm::url`
Expected: PASS for all 10 cases.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/swarm/mod.rs src/swarm/url.rs
git commit -m "feat(swarm): URL parser for /reviews/N and /changes/N"
```

---

## Task 2: `read_only` field on sessions

**Files:**
- Modify: `src/session.rs`

- [ ] **Step 1: Write failing test for read-only guard**

Append to `src/session.rs`'s existing `mod tests` (find it — there are tests at the bottom; if there isn't one, add one):

```rust
#[test]
fn set_side_text_no_op_when_read_only() {
    let store = SessionStore::new();
    let id = store.open_two_way("a\n", "b\n", None).unwrap();
    store.with(id, |s| { s.read_only = true; Ok(()) }).unwrap();
    let res = store.set_side_text(id, SideRef::TwoWay(TwoWaySide::A), "changed".into());
    assert!(matches!(res, Err(SessionError::ReadOnly)));
    let snap = store.snapshot(id).unwrap();
    let SessionMode::TwoWay { a_text, .. } = snap.mode else { panic!() };
    assert_eq!(a_text, "a");
}
```

- [ ] **Step 2: Run test, expect compile failure**

Run: `cargo test --no-default-features --lib session::tests::set_side_text_no_op_when_read_only`
Expected: FAIL — `no field 'read_only'`, `no variant 'ReadOnly'`.

- [ ] **Step 3: Add the field + error variant**

In `src/session.rs`:

1. Add `pub read_only: bool,` to `DiffSession` (after `manual_result`).
2. Find `SessionError` (the `#[derive(Debug, thiserror::Error)]` enum near line ~100) and add:

```rust
#[error("session is read-only")]
ReadOnly,
```

3. In every `DiffSession { ... }` literal (search for `manual_result: None,` — there are two, in `open_two_way_with` and `open_three_way_with`) add `read_only: false,` directly after.

4. At the top of `set_side_text` body, after the `let s = sessions.get_mut(...)` line:

```rust
if s.read_only {
    return Err(SessionError::ReadOnly);
}
```

- [ ] **Step 4: Run the new test**

Run: `cargo test --no-default-features --lib session::tests::set_side_text_no_op_when_read_only`
Expected: PASS.

- [ ] **Step 5: Run the full core test suite to confirm no regressions**

Run: `cargo test --no-default-features --lib`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add src/session.rs
git commit -m "feat(session): add read_only flag; set_side_text returns ReadOnly"
```

---

## Task 3: Swarm models (serde DTOs)

**Files:**
- Create: `src/swarm/model.rs`
- Modify: `src/swarm/mod.rs`
- Create: `tests/swarm_fixtures/review.json`
- Create: `tests/swarm_fixtures/change.json`

- [ ] **Step 1: Capture realistic fixtures**

Create `tests/swarm_fixtures/review.json` with a minimal real-shape Swarm response:

```json
{
  "review": {
    "id": 12345,
    "author": "alice",
    "description": "Fix the foo widget so it doesn't bar.\n",
    "state": "needsReview",
    "created": 1715000000,
    "updated": 1715100000,
    "participants": {
      "bob": { "vote": { "value": 1 } },
      "carol": { "vote": { "value": 0 } }
    },
    "versions": [
      { "change": 99001, "user": "alice" },
      { "change": 99002, "user": "alice" }
    ]
  },
  "files": [
    { "depotFile": "//depot/foo.rs", "action": "edit", "rev": "3", "fileSize": 1234, "type": "text" },
    { "depotFile": "//depot/added.rs", "action": "add", "rev": "1", "fileSize": 200, "type": "text" },
    { "depotFile": "//depot/gone.rs", "action": "delete", "rev": "5", "fileSize": 0, "type": "text" },
    { "depotFile": "//depot/img.png", "action": "edit", "rev": "2", "fileSize": 50000, "type": "binary" }
  ]
}
```

Create `tests/swarm_fixtures/change.json`:

```json
{
  "change": {
    "change": 99002,
    "user": "alice",
    "description": "Quick fix for the bar regression.\n",
    "status": "submitted",
    "time": 1715100000
  },
  "files": [
    { "depotFile": "//depot/bar.rs", "action": "edit", "rev": "8", "fileSize": 500, "type": "text" }
  ]
}
```

- [ ] **Step 2: Write failing tests for model deserialization**

Create `src/swarm/model.rs`:

```rust
//! Serde DTOs for Swarm API responses. Only the fields Diffie consumes are
//! modeled; unknown fields are ignored.

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind { Review, Change }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileAction {
    Add,
    Edit,
    Delete,
    Rename { from: String },
    Branch,
    Integrate,
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub depot_path: String,
    pub action: FileAction,
    /// Revision number of the file *before* this change (None for adds).
    pub rev_pre: Option<u32>,
    /// Revision number of the file *after* this change (None for deletes).
    pub rev_post: Option<u32>,
    /// `true` for text files, `false` for binary/unknown.
    pub is_text: bool,
}

#[derive(Debug, Clone)]
pub struct ReviewMeta {
    pub id: u64,
    pub kind: TargetKind,
    pub description: String,
    pub author: String,
    pub state: String,
    pub created: i64,
    pub updated: Option<i64>,
    pub participants: Vec<Participant>,
    /// Canonical web URL — used by the "Open in browser" button.
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct Participant {
    pub user: String,
    pub vote: i32,
}

// -- raw response shapes -----------------------------------------------

#[derive(Deserialize)]
pub(crate) struct ReviewResponse {
    pub review: RawReview,
    pub files: Vec<RawFile>,
}

#[derive(Deserialize)]
pub(crate) struct ChangeResponse {
    pub change: RawChange,
    pub files: Vec<RawFile>,
}

#[derive(Deserialize)]
pub(crate) struct RawReview {
    pub id: u64,
    pub author: String,
    pub description: String,
    pub state: String,
    pub created: i64,
    pub updated: Option<i64>,
    #[serde(default)]
    pub participants: std::collections::BTreeMap<String, RawParticipant>,
}

#[derive(Deserialize, Default)]
pub(crate) struct RawParticipant {
    #[serde(default)]
    pub vote: Option<RawVote>,
}

#[derive(Deserialize)]
pub(crate) struct RawVote { pub value: i32 }

#[derive(Deserialize)]
pub(crate) struct RawChange {
    pub change: u64,
    pub user: String,
    pub description: String,
    pub status: String,
    pub time: i64,
}

#[derive(Deserialize)]
pub(crate) struct RawFile {
    #[serde(rename = "depotFile")]
    pub depot_path: String,
    pub action: String,
    pub rev: String,
    #[serde(default, rename = "type")]
    pub file_type: Option<String>,
    #[serde(default, rename = "fromFile")]
    pub from_file: Option<String>,
}

impl RawFile {
    pub(crate) fn into_entry(self) -> FileEntry {
        let rev_post: Option<u32> = self.rev.parse().ok();
        let (action, has_pre) = match self.action.as_str() {
            "add" => (FileAction::Add, false),
            "delete" => (FileAction::Delete, true),
            "edit" => (FileAction::Edit, true),
            "branch" => (FileAction::Branch, false),
            "integrate" => (FileAction::Integrate, true),
            "move/add" | "move/delete" if self.from_file.is_some() => (
                FileAction::Rename { from: self.from_file.clone().unwrap() },
                true,
            ),
            _ => (FileAction::Edit, true),
        };
        let rev_pre = if has_pre { rev_post.map(|r| r.saturating_sub(1)).filter(|r| *r > 0) } else { None };
        let rev_post = if matches!(action, FileAction::Delete) { None } else { rev_post };
        let is_text = matches!(self.file_type.as_deref(), Some(t) if t.contains("text") || t == "unicode")
            || self.file_type.is_none();
        FileEntry { depot_path: self.depot_path, action, rev_pre, rev_post, is_text }
    }
}

impl ReviewResponse {
    pub(crate) fn into_meta_and_files(self, host: &str) -> (ReviewMeta, Vec<FileEntry>) {
        let r = self.review;
        let participants = r
            .participants
            .into_iter()
            .map(|(user, p)| Participant { user, vote: p.vote.map(|v| v.value).unwrap_or(0) })
            .collect();
        let meta = ReviewMeta {
            id: r.id,
            kind: TargetKind::Review,
            description: r.description,
            author: r.author,
            state: r.state,
            created: r.created,
            updated: r.updated,
            participants,
            url: format!("{host}/reviews/{}", r.id),
        };
        let files = self.files.into_iter().map(RawFile::into_entry).collect();
        (meta, files)
    }
}

impl ChangeResponse {
    pub(crate) fn into_meta_and_files(self, host: &str) -> (ReviewMeta, Vec<FileEntry>) {
        let c = self.change;
        let meta = ReviewMeta {
            id: c.change,
            kind: TargetKind::Change,
            description: c.description,
            author: c.user,
            state: c.status,
            created: c.time,
            updated: None,
            participants: vec![],
            url: format!("{host}/changes/{}", c.change),
        };
        let files = self.files.into_iter().map(RawFile::into_entry).collect();
        (meta, files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVIEW_FIXTURE: &str = include_str!("../../tests/swarm_fixtures/review.json");
    const CHANGE_FIXTURE: &str = include_str!("../../tests/swarm_fixtures/change.json");

    #[test]
    fn parses_review_fixture() {
        let r: ReviewResponse = serde_json::from_str(REVIEW_FIXTURE).unwrap();
        let (meta, files) = r.into_meta_and_files("https://swarm.example.com");
        assert_eq!(meta.id, 12345);
        assert_eq!(meta.author, "alice");
        assert_eq!(meta.state, "needsReview");
        assert!(meta.description.contains("foo widget"));
        assert_eq!(meta.participants.len(), 2);
        assert_eq!(meta.url, "https://swarm.example.com/reviews/12345");

        assert_eq!(files.len(), 4);
        assert!(matches!(files[0].action, FileAction::Edit));
        assert_eq!(files[0].rev_pre, Some(2));
        assert_eq!(files[0].rev_post, Some(3));
        assert!(files[0].is_text);

        assert!(matches!(files[1].action, FileAction::Add));
        assert_eq!(files[1].rev_pre, None);

        assert!(matches!(files[2].action, FileAction::Delete));
        assert_eq!(files[2].rev_post, None);

        assert!(matches!(files[3].action, FileAction::Edit));
        assert!(!files[3].is_text);
    }

    #[test]
    fn parses_change_fixture() {
        let r: ChangeResponse = serde_json::from_str(CHANGE_FIXTURE).unwrap();
        let (meta, files) = r.into_meta_and_files("https://swarm.example.com");
        assert_eq!(meta.id, 99002);
        assert!(matches!(meta.kind, TargetKind::Change));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].depot_path, "//depot/bar.rs");
    }
}
```

- [ ] **Step 3: Wire module**

Edit `src/swarm/mod.rs`:

```rust
//! Helix Swarm integration.

pub mod url;
pub mod model;
```

- [ ] **Step 4: Run tests**

Run: `cargo test --no-default-features --lib swarm::model`
Expected: both PASS.

- [ ] **Step 5: Commit**

```bash
git add src/swarm/mod.rs src/swarm/model.rs tests/swarm_fixtures/
git commit -m "feat(swarm): DTOs for review/change with fixture-based tests"
```

---

## Task 4: Swarm client trait + ureq implementation

**Files:**
- Create: `src/swarm/client.rs`
- Modify: `src/swarm/mod.rs`
- Modify: `Cargo.toml`

- [ ] **Step 1: Add `ureq` to gui feature**

In `Cargo.toml`:

1. Add to the `gui = [ ... ]` array entries:
   ```toml
       "dep:ureq",
   ```
2. Under `[dependencies]` add (note: optional so it only ships with `gui`):
   ```toml
   ureq = { version = "2", default-features = false, features = ["tls", "json"], optional = true }
   ```

- [ ] **Step 2: Define the trait and Error type**

Create `src/swarm/client.rs`:

```rust
//! HTTP client for Swarm's REST API. The [`SwarmApi`] trait is the seam
//! used by the loader so tests can substitute a fake.

use crate::swarm::model::{ChangeResponse, FileEntry, ReviewMeta, ReviewResponse};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("authentication failed")]
    Auth,
    #[error("not found")]
    NotFound,
    #[error("network: {0}")]
    Network(String),
    #[error("decode: {0}")]
    Decode(String),
}

pub trait SwarmApi: Send + Sync {
    fn login(&self, user: &str, password: &str) -> Result<String, Error>;
    fn get_review(&self, id: u64) -> Result<(ReviewMeta, Vec<FileEntry>), Error>;
    fn get_change(&self, id: u64) -> Result<(ReviewMeta, Vec<FileEntry>), Error>;
    /// Fetches the raw bytes of a depot file at the given revision.
    fn get_file_content(&self, depot_path: &str, rev: u32) -> Result<Vec<u8>, Error>;
    /// Cheap authenticated probe (used to detect expired tickets without prompting).
    fn probe(&self) -> Result<(), Error>;
}

#[cfg(feature = "gui")]
pub use ureq_impl::Client;

#[cfg(feature = "gui")]
mod ureq_impl {
    use super::*;
    use crate::swarm::model::{ChangeResponse, ReviewResponse};

    pub struct Client {
        agent: ureq::Agent,
        host: String,
        user: String,
        ticket: String,
    }

    impl Client {
        pub fn new(host: impl Into<String>, user: impl Into<String>, ticket: impl Into<String>) -> Self {
            Self { agent: ureq::AgentBuilder::new().build(), host: host.into(), user: user.into(), ticket: ticket.into() }
        }

        fn auth(&self) -> String {
            use base64::Engine;
            let raw = format!("{}:{}", self.user, self.ticket);
            format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(raw))
        }

        fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
            let url = format!("{}{}", self.host, path);
            let res = self.agent.get(&url).set("Authorization", &self.auth()).call();
            match res {
                Ok(r) => r.into_json::<T>().map_err(|e| Error::Decode(e.to_string())),
                Err(ureq::Error::Status(401, _)) => Err(Error::Auth),
                Err(ureq::Error::Status(404, _)) => Err(Error::NotFound),
                Err(e) => Err(Error::Network(e.to_string())),
            }
        }
    }

    impl SwarmApi for Client {
        fn login(&self, user: &str, password: &str) -> Result<String, Error> {
            let url = format!("{}/api/v9/login", self.host);
            let res = self
                .agent
                .post(&url)
                .send_json(serde_json::json!({ "user": user, "password": password }));
            match res {
                Ok(r) => {
                    let v: serde_json::Value = r.into_json().map_err(|e| Error::Decode(e.to_string()))?;
                    v.get("ticket")
                        .or_else(|| v.get("user").and_then(|u| u.get("ticket")))
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                        .ok_or_else(|| Error::Decode("no ticket in response".into()))
                }
                Err(ureq::Error::Status(401, _)) => Err(Error::Auth),
                Err(e) => Err(Error::Network(e.to_string())),
            }
        }

        fn get_review(&self, id: u64) -> Result<(ReviewMeta, Vec<FileEntry>), Error> {
            let resp: ReviewResponse = self.get_json(&format!("/api/v9/reviews/{id}"))?;
            Ok(resp.into_meta_and_files(&self.host))
        }

        fn get_change(&self, id: u64) -> Result<(ReviewMeta, Vec<FileEntry>), Error> {
            let resp: ChangeResponse = self.get_json(&format!("/api/v9/changes/{id}"))?;
            Ok(resp.into_meta_and_files(&self.host))
        }

        fn get_file_content(&self, depot_path: &str, rev: u32) -> Result<Vec<u8>, Error> {
            // url-encode the depot path; keep '/' so the segments are visible.
            let encoded: String = depot_path.chars().map(|c| match c {
                '/' | 'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => c.to_string(),
                _ => format!("%{:02X}", c as u32),
            }).collect();
            let url = format!("{}/api/v10/files/{encoded}?rev={rev}&fields=content", self.host);
            let res = self.agent.get(&url).set("Authorization", &self.auth()).call();
            match res {
                Ok(r) => {
                    let mut buf = Vec::new();
                    r.into_reader().read_to_end(&mut buf).map_err(|e| Error::Network(e.to_string()))?;
                    Ok(buf)
                }
                Err(ureq::Error::Status(401, _)) => Err(Error::Auth),
                Err(ureq::Error::Status(404, _)) => Err(Error::NotFound),
                Err(e) => Err(Error::Network(e.to_string())),
            }
        }

        fn probe(&self) -> Result<(), Error> {
            let url = format!("{}/api/v9/projects?max=1", self.host);
            let res = self.agent.get(&url).set("Authorization", &self.auth()).call();
            match res {
                Ok(_) => Ok(()),
                Err(ureq::Error::Status(401, _)) => Err(Error::Auth),
                Err(e) => Err(Error::Network(e.to_string())),
            }
        }
    }

    use std::io::Read;
}
```

- [ ] **Step 3: Add `base64` dep**

In `Cargo.toml`, under `[dependencies]`:

```toml
base64 = { version = "0.22", optional = true }
```

And add `"dep:base64"` to the `gui = [ ... ]` array.

- [ ] **Step 4: Wire module**

Edit `src/swarm/mod.rs`:

```rust
//! Helix Swarm integration.

pub mod url;
pub mod model;
pub mod client;
```

- [ ] **Step 5: Build to verify**

Run: `cargo build`
Expected: success. (No tests yet — the trait is exercised in Task 6 via a fake.)

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/swarm/mod.rs src/swarm/client.rs
git commit -m "feat(swarm): SwarmApi trait + ureq Client with Basic auth"
```

---

## Task 5: Background loader

**Files:**
- Create: `src/swarm/loader.rs`
- Modify: `src/swarm/mod.rs`

- [ ] **Step 1: Define events and handle**

Create `src/swarm/loader.rs`:

```rust
//! Background orchestrator. Spawns a thread that fetches review/change
//! metadata then fans out to per-file content fetches. Reports progress
//! through an mpsc channel drained by the App on each frame.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use crate::swarm::client::SwarmApi;
use crate::swarm::model::{FileAction, FileEntry, ReviewMeta};
use crate::swarm::url::{SwarmTarget, SwarmUrl};

#[derive(Debug, Clone)]
pub enum SidePayload {
    /// UTF-8 text plus whether it had a trailing newline.
    Text(String, bool),
    /// File is binary (no diff possible).
    Binary,
    /// Side is intentionally empty (other side is the only one — add or delete).
    Empty,
}

#[derive(Debug)]
pub enum LoaderEvent {
    MetaReady(ReviewMeta),
    FileTotalKnown(usize),
    FileReady {
        entry: FileEntry,
        left: SidePayload,
        right: SidePayload,
    },
    FileFailed { depot_path: String, error: String },
    AllDone,
}

pub struct LoaderHandle {
    pub rx: Receiver<LoaderEvent>,
    _join: thread::JoinHandle<()>,
}

pub fn spawn(api: Arc<dyn SwarmApi>, url: SwarmUrl) -> LoaderHandle {
    let (tx, rx) = channel();
    let join = thread::spawn(move || run(api, url, tx));
    LoaderHandle { rx, _join: join }
}

fn run(api: Arc<dyn SwarmApi>, url: SwarmUrl, tx: Sender<LoaderEvent>) {
    let result = match url.target {
        SwarmTarget::Review(id) => api.get_review(id),
        SwarmTarget::Change(id) => api.get_change(id),
    };
    let (meta, files) = match result {
        Ok(v) => v,
        Err(e) => {
            let _ = tx.send(LoaderEvent::FileFailed {
                depot_path: String::new(),
                error: format!("metadata: {e}"),
            });
            let _ = tx.send(LoaderEvent::AllDone);
            return;
        }
    };
    let _ = tx.send(LoaderEvent::MetaReady(meta));
    let _ = tx.send(LoaderEvent::FileTotalKnown(files.len()));

    // Fan out up to 4 workers. Each pulls from a shared queue.
    let queue = Arc::new(std::sync::Mutex::new(files.into_iter().collect::<Vec<_>>()));
    let mut workers = Vec::new();
    for _ in 0..4 {
        let api = api.clone();
        let tx = tx.clone();
        let queue = queue.clone();
        workers.push(thread::spawn(move || {
            loop {
                let entry = { queue.lock().unwrap().pop() };
                let Some(entry) = entry else { break };
                let (left, right) = fetch_sides(&*api, &entry);
                let _ = tx.send(match (&left, &right) {
                    (Err(e), _) | (_, Err(e)) => LoaderEvent::FileFailed {
                        depot_path: entry.depot_path.clone(),
                        error: e.clone(),
                    },
                    _ => LoaderEvent::FileReady {
                        entry,
                        left: left.unwrap_or(SidePayload::Empty),
                        right: right.unwrap_or(SidePayload::Empty),
                    },
                });
            }
        }));
    }
    for w in workers { let _ = w.join(); }
    let _ = tx.send(LoaderEvent::AllDone);
}

fn fetch_sides(
    api: &dyn SwarmApi,
    entry: &FileEntry,
) -> (Result<SidePayload, String>, Result<SidePayload, String>) {
    if !entry.is_text {
        let left = if entry.rev_pre.is_some() { Ok(SidePayload::Binary) } else { Ok(SidePayload::Empty) };
        let right = if entry.rev_post.is_some() { Ok(SidePayload::Binary) } else { Ok(SidePayload::Empty) };
        return (left, right);
    }
    let left_path = match &entry.action {
        FileAction::Rename { from } => from.as_str(),
        _ => entry.depot_path.as_str(),
    };
    let left = match entry.rev_pre {
        Some(rev) => fetch_text(api, left_path, rev),
        None => Ok(SidePayload::Empty),
    };
    let right = match entry.rev_post {
        Some(rev) => fetch_text(api, &entry.depot_path, rev),
        None => Ok(SidePayload::Empty),
    };
    (left, right)
}

fn fetch_text(api: &dyn SwarmApi, depot_path: &str, rev: u32) -> Result<SidePayload, String> {
    let bytes = api.get_file_content(depot_path, rev).map_err(|e| e.to_string())?;
    let (cow, _, had_errors) = encoding_rs::UTF_8.decode(&bytes);
    if had_errors {
        let (cow2, _, _) = encoding_rs::WINDOWS_1252.decode(&bytes);
        let text = cow2.into_owned();
        let trailing = text.ends_with('\n');
        let trimmed = text.trim_end_matches('\n').to_string();
        return Ok(SidePayload::Text(trimmed, trailing));
    }
    let text = cow.into_owned();
    let trailing = text.ends_with('\n');
    Ok(SidePayload::Text(text.trim_end_matches('\n').to_string(), trailing))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::client::Error as ClientError;
    use crate::swarm::model::TargetKind;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct FakeApi {
        meta: ReviewMeta,
        files: Vec<FileEntry>,
        bodies: Mutex<HashMap<(String, u32), Vec<u8>>>,
    }

    impl SwarmApi for FakeApi {
        fn login(&self, _u: &str, _p: &str) -> Result<String, ClientError> { unimplemented!() }
        fn probe(&self) -> Result<(), ClientError> { Ok(()) }
        fn get_review(&self, _id: u64) -> Result<(ReviewMeta, Vec<FileEntry>), ClientError> {
            Ok((self.meta.clone(), self.files.clone()))
        }
        fn get_change(&self, _id: u64) -> Result<(ReviewMeta, Vec<FileEntry>), ClientError> {
            Ok((self.meta.clone(), self.files.clone()))
        }
        fn get_file_content(&self, p: &str, r: u32) -> Result<Vec<u8>, ClientError> {
            self.bodies.lock().unwrap()
                .get(&(p.to_string(), r))
                .cloned()
                .ok_or(ClientError::NotFound)
        }
    }

    #[test]
    fn loader_emits_meta_then_files_then_done() {
        let meta = ReviewMeta {
            id: 1, kind: TargetKind::Review,
            description: "x".into(), author: "a".into(), state: "s".into(),
            created: 0, updated: None, participants: vec![], url: "http://h/reviews/1".into(),
        };
        let files = vec![FileEntry {
            depot_path: "//f.rs".into(),
            action: FileAction::Edit,
            rev_pre: Some(1), rev_post: Some(2),
            is_text: true,
        }];
        let mut bodies = HashMap::new();
        bodies.insert(("//f.rs".to_string(), 1), b"before\n".to_vec());
        bodies.insert(("//f.rs".to_string(), 2), b"after\n".to_vec());
        let api: Arc<dyn SwarmApi> = Arc::new(FakeApi { meta, files, bodies: Mutex::new(bodies) });
        let handle = spawn(api, SwarmUrl { host: "http://h".into(), target: SwarmTarget::Review(1) });

        let mut got_meta = false;
        let mut got_file = false;
        let mut got_done = false;
        while let Ok(ev) = handle.rx.recv() {
            match ev {
                LoaderEvent::MetaReady(_) => got_meta = true,
                LoaderEvent::FileReady { left, right, .. } => {
                    got_file = true;
                    assert!(matches!(left, SidePayload::Text(ref t, true) if t == "before"));
                    assert!(matches!(right, SidePayload::Text(ref t, true) if t == "after"));
                }
                LoaderEvent::AllDone => { got_done = true; break; }
                _ => {}
            }
        }
        assert!(got_meta && got_file && got_done);
    }
}
```

- [ ] **Step 2: Wire module**

Edit `src/swarm/mod.rs`:

```rust
//! Helix Swarm integration.

pub mod url;
pub mod model;
pub mod client;
pub mod loader;
```

- [ ] **Step 3: Run loader tests**

Run: `cargo test --lib swarm::loader`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/swarm/loader.rs src/swarm/mod.rs
git commit -m "feat(swarm): background loader with parallel file fetches"
```

---

## Task 6: Keychain credential helper

**Files:**
- Create: `src/app/swarm_creds.rs`
- Modify: `src/app/mod.rs` (declare module)
- Modify: `Cargo.toml`

- [ ] **Step 1: Add `keyring` dep under gui feature**

In `Cargo.toml`:

```toml
keyring = { version = "3", optional = true, features = ["apple-native", "windows-native", "sync-secret-service"] }
```

Add `"dep:keyring"` to the `gui = [ ... ]` array.

- [ ] **Step 2: Implement helpers**

Create `src/app/swarm_creds.rs`:

```rust
//! OS-keychain storage for Swarm credentials.
//!
//! Per-host the service "diffie-swarm" holds two entries:
//!   account "{host}:user"   -> username
//!   account "{host}:ticket" -> P4 ticket (used as Basic-Auth password)
//!
//! All operations are best-effort: if the platform keychain is unavailable
//! the helpers degrade to None/no-op rather than crashing the GUI.

const SERVICE: &str = "diffie-swarm";

fn entry(host: &str, kind: &str) -> Option<keyring::Entry> {
    keyring::Entry::new(SERVICE, &format!("{host}:{kind}")).ok()
}

pub fn load(host: &str) -> Option<(String, String)> {
    let user = entry(host, "user")?.get_password().ok()?;
    let ticket = entry(host, "ticket")?.get_password().ok()?;
    Some((user, ticket))
}

pub fn store(host: &str, user: &str, ticket: &str) {
    if let Some(e) = entry(host, "user") { let _ = e.set_password(user); }
    if let Some(e) = entry(host, "ticket") { let _ = e.set_password(ticket); }
}

pub fn clear_ticket(host: &str) {
    if let Some(e) = entry(host, "ticket") { let _ = e.delete_credential(); }
}

pub fn load_user(host: &str) -> Option<String> {
    entry(host, "user")?.get_password().ok()
}
```

- [ ] **Step 3: Register module**

In `src/app/mod.rs`, find the block of `mod xxx;` declarations near the top and add:

```rust
mod swarm_creds;
```

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: success.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/app/swarm_creds.rs src/app/mod.rs
git commit -m "feat(app): keyring-backed Swarm credential storage"
```

---

## Task 7: `InitialOpen::Swarm` + main.rs single-arg routing

**Files:**
- Modify: `src/app/mod.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Add InitialOpen variant**

In `src/app/mod.rs`, find `pub enum InitialOpen` and add:

```rust
    Swarm(crate::swarm::url::SwarmUrl),
```

- [ ] **Step 2: Stub apply branch (will be filled in later tasks)**

Locate the `match initial { ... InitialOpen::TwoWay { ... } InitialOpen::ThreeWay { ... } }` block (around `src/app/mod.rs:858`) and add:

```rust
            InitialOpen::Swarm(url) => { state.pending_swarm = Some(url); }
```

Add a new field to `AppState` (near the other Swarm-related fields we'll add in Task 8):

```rust
    pending_swarm: Option<crate::swarm::url::SwarmUrl>,
```

and initialize it `None` in the `Default` impl (or wherever `AppState` is constructed — search for `AppState {`).

- [ ] **Step 3: Route the single-arg case in main.rs**

Edit `src/main.rs`. Replace the existing `match args.len()` with:

```rust
    let initial = match args.len() {
        0 => None,
        1 => match diffie_lib::swarm::url::parse(args[0]) {
            Ok(u) => Some(diffie_lib::app::InitialOpen::Swarm(u)),
            Err(_) => {
                print_usage(prog, &mut std::io::stderr());
                std::process::exit(2);
            }
        },
        2 => Some(diffie_lib::app::InitialOpen::TwoWay {
            a: PathBuf::from(args[0]),
            b: PathBuf::from(args[1]),
        }),
        4 => Some(diffie_lib::app::InitialOpen::ThreeWay {
            base: PathBuf::from(args[0]),
            local: PathBuf::from(args[1]),
            remote: PathBuf::from(args[2]),
            result: PathBuf::from(args[3]),
        }),
        _ => {
            print_usage(prog, &mut std::io::stderr());
            std::process::exit(2);
        }
    };
```

Also extend `print_usage`:

```rust
    let _ = writeln!(out, "  {prog} <swarm-url>                       Open a Swarm review or changelist");
```

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: success.

- [ ] **Step 5: Smoke-test the URL rejection path**

Run: `cargo run -- "not a url"`
Expected: prints usage and exits with code 2.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/app/mod.rs
git commit -m "feat(cli): single-arg Swarm URL routes to InitialOpen::Swarm"
```

---

## Task 8: Swarm login modal

**Files:**
- Create: `src/app/swarm_login.rs`
- Modify: `src/app/mod.rs`

- [ ] **Step 1: Add SwarmAuth state, loader handle, progress fields**

In `src/app/mod.rs`, add to `AppState`:

```rust
    swarm_auth: Option<swarm_login::SwarmAuth>,
    swarm_loader: Option<crate::swarm::loader::LoaderHandle>,
    swarm_progress: Option<(usize, usize)>,
    swarm_info_meta: std::collections::HashMap<SessionId, crate::swarm::model::ReviewMeta>,
```

Initialize each to `None` / empty in the constructor.

- [ ] **Step 2: Declare module**

Add to the `mod xxx;` block in `src/app/mod.rs`:

```rust
mod swarm_login;
```

- [ ] **Step 3: Implement the modal state machine**

Create `src/app/swarm_login.rs`:

```rust
//! Modal login dialog shown on launch when the user supplied a Swarm URL.
//!
//! Owns a small state machine:
//!   Idle      -> user is editing the form
//!   Probing   -> background thread is verifying a cached ticket
//!   LoggingIn -> background thread is exchanging password for ticket
//!   Done      -> credentials ready (App reads + transitions out)
//!   Cancelled -> user hit Cancel; App should exit

use std::sync::mpsc::{Receiver, channel};
use std::thread;

use crate::swarm::client::{self, Client, SwarmApi};
use crate::swarm::url::SwarmUrl;

#[derive(Debug)]
pub enum SwarmAuth {
    Pending {
        url: SwarmUrl,
        user_input: String,
        password_input: String,
        error: Option<String>,
        rx: Option<Receiver<Result<(String, String), String>>>, // (user, ticket) or error
    },
    Ready { url: SwarmUrl, user: String, ticket: String },
    Cancelled,
}

impl SwarmAuth {
    /// Build the initial state, kicking off a probe if cached credentials exist.
    pub fn new(url: SwarmUrl) -> Self {
        let prefill_user = super::swarm_creds::load_user(&url.host).unwrap_or_default();
        let mut state = SwarmAuth::Pending {
            url: url.clone(),
            user_input: prefill_user,
            password_input: String::new(),
            error: None,
            rx: None,
        };
        if let Some((user, ticket)) = super::swarm_creds::load(&url.host) {
            // Spawn a probe in the background; on success skip the modal entirely.
            let (tx, rx) = channel();
            let host = url.host.clone();
            let user2 = user.clone();
            let ticket2 = ticket.clone();
            thread::spawn(move || {
                let c = Client::new(host, user2.clone(), ticket2.clone());
                let res = c.probe().map(|_| (user2, ticket2)).map_err(|e| e.to_string());
                let _ = tx.send(res);
            });
            if let SwarmAuth::Pending { rx: r, .. } = &mut state {
                *r = Some(rx);
            }
        }
        state
    }
}

/// Render the modal. Returns `true` if the App should consume the auth
/// result (state advanced to Ready or Cancelled).
pub fn render(ui: &imgui::Ui, auth: &mut SwarmAuth) -> bool {
    // Try to advance a pending background result first.
    if let SwarmAuth::Pending { rx, .. } = auth {
        if let Some(r) = rx.as_ref() {
            if let Ok(result) = r.try_recv() {
                match result {
                    Ok((user, ticket)) => {
                        let url = match auth {
                            SwarmAuth::Pending { url, .. } => url.clone(),
                            _ => unreachable!(),
                        };
                        super::swarm_creds::store(&url.host, &user, &ticket);
                        *auth = SwarmAuth::Ready { url, user, ticket };
                        return true;
                    }
                    Err(e) => {
                        if let SwarmAuth::Pending { error, rx, .. } = auth {
                            *error = Some(e);
                            *rx = None;
                        }
                    }
                }
            }
        }
    }

    let SwarmAuth::Pending { url, user_input, password_input, error, rx } = auth else {
        return false;
    };

    let busy = rx.is_some();
    let mut want_login = false;
    let mut want_cancel = false;

    ui.open_popup("Swarm login");
    if let Some(_t) = ui.modal_popup_config("Swarm login")
        .always_auto_resize(true)
        .resizable(false)
        .begin_popup()
    {
        ui.text(format!("Host: {}", url.host));
        ui.separator();
        ui.input_text("Username", user_input).build();
        ui.input_text("Password", password_input).password(true).build();
        if let Some(e) = error.as_deref() {
            ui.text_colored([1.0, 0.4, 0.4, 1.0], e);
        }
        if busy { ui.text("Logging in…"); }
        ui.separator();
        if !busy {
            if ui.button("Login") { want_login = true; }
            ui.same_line();
            if ui.button("Cancel") { want_cancel = true; }
        }
    }

    if want_login {
        let (tx, new_rx) = channel();
        let host = url.host.clone();
        let user = user_input.clone();
        let pw = password_input.clone();
        thread::spawn(move || {
            let c = Client::new(host.clone(), String::new(), String::new());
            let res = c.login(&user, &pw)
                .map(|t| (user, t))
                .map_err(|e| e.to_string());
            let _ = tx.send(res);
        });
        *rx = Some(new_rx);
        *error = None;
        password_input.clear();
    }

    if want_cancel {
        *auth = SwarmAuth::Cancelled;
        return true;
    }

    false
}
```

- [ ] **Step 4: Bootstrap auth state when pending_swarm is set**

In `src/app/mod.rs`, find `frame_ui` (search for `fn frame_ui` or the spot where `pending_initial` would be consumed — search for `pending_initial`). Add near the start of `frame_ui`:

```rust
    // Promote pending Swarm URL into an auth state machine.
    if let Some(url) = state.pending_swarm.take() {
        state.swarm_auth = Some(swarm_login::SwarmAuth::new(url));
    }

    // Drive the Swarm login modal if active.
    if let Some(auth) = state.swarm_auth.as_mut() {
        let consumed = swarm_login::render(ui, auth);
        if consumed {
            match state.swarm_auth.take().unwrap() {
                swarm_login::SwarmAuth::Ready { url, user, ticket } => {
                    let api: std::sync::Arc<dyn crate::swarm::client::SwarmApi> =
                        std::sync::Arc::new(crate::swarm::client::Client::new(
                            url.host.clone(), user, ticket,
                        ));
                    state.swarm_loader = Some(crate::swarm::loader::spawn(api, url));
                }
                swarm_login::SwarmAuth::Cancelled => state.quit_requested = true,
                swarm_login::SwarmAuth::Pending { .. } => {} // shouldn't happen
            }
        }
    }
```

- [ ] **Step 5: Build**

Run: `cargo build`
Expected: success. (Modal renders only when launched with a Swarm URL.)

- [ ] **Step 6: Commit**

```bash
git add src/app/swarm_login.rs src/app/mod.rs
git commit -m "feat(app): Swarm login modal with cached-ticket probe"
```

---

## Task 9: TabMode::SwarmInfo + info-tab view

**Files:**
- Create: `src/app/swarm_info_view.rs`
- Modify: `src/app/mod.rs`
- Modify: `Cargo.toml`

- [ ] **Step 1: Add `open` dep (gui-gated)**

In `Cargo.toml`:

```toml
open = { version = "5", optional = true }
```

Add `"dep:open"` to the `gui = [ ... ]` array.

- [ ] **Step 2: Add the SwarmInfo tab variant**

In `src/app/mod.rs`, edit `enum TabMode`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TabMode {
    TwoWay,
    ThreeWay,
    SwarmInfo,
}
```

- [ ] **Step 3: Implement the info view**

Create `src/app/swarm_info_view.rs`:

```rust
//! Read-only info tab for a Swarm review/changelist. Displays metadata,
//! a progress bar while files are still loading, the file list (clickable
//! to switch to that file's tab), reviewers/votes (reviews only), and an
//! "Open in browser" button.

use imgui::Ui;

use crate::swarm::model::{ReviewMeta, TargetKind};

pub struct InfoContext<'a> {
    pub meta: &'a ReviewMeta,
    pub progress: Option<(usize, usize)>,
    /// File-list rows the user can click to jump to that file's tab.
    pub file_rows: &'a [InfoFileRow],
    /// Index in `file_rows` the user clicked, if any.
    pub click_index: &'a mut Option<usize>,
    pub open_in_browser: &'a mut bool,
}

pub struct InfoFileRow {
    pub depot_path: String,
    pub action_label: String,
    /// `None` if the tab hasn't been created yet (still loading).
    pub session_id: Option<crate::session::SessionId>,
}

pub fn render(ui: &Ui, ctx: InfoContext<'_>) {
    let title = match ctx.meta.kind {
        TargetKind::Review => format!("Review #{}", ctx.meta.id),
        TargetKind::Change => format!("Change #{}", ctx.meta.id),
    };
    ui.text(&title);
    ui.same_line();
    ui.text_disabled(format!("  {}  ", ctx.meta.state));
    ui.same_line();
    ui.text(format!("by {}", ctx.meta.author));
    ui.same_line();
    if ui.button("Open in browser") { *ctx.open_in_browser = true; }

    ui.separator();
    if let Some((done, total)) = ctx.progress {
        if done < total {
            imgui::ProgressBar::new(done as f32 / total.max(1) as f32)
                .overlay_text(format!("Loaded {done} / {total}"))
                .build(ui);
        } else {
            ui.text_disabled(format!("Loaded {total} files"));
        }
        ui.separator();
    }

    ui.text("Description:");
    let mut desc = ctx.meta.description.clone();
    ui.input_text_multiline("##desc", &mut desc, [-1.0, 120.0])
        .read_only(true)
        .build();

    if matches!(ctx.meta.kind, TargetKind::Review) && !ctx.meta.participants.is_empty() {
        ui.separator();
        ui.text("Reviewers:");
        for p in &ctx.meta.participants {
            let v = match p.vote.signum() {
                1 => "+1",
                -1 => "-1",
                _ => " 0",
            };
            ui.text(format!("  {v}  {}", p.user));
        }
    }

    ui.separator();
    ui.text("Files:");
    if let Some(_t) = ui.begin_table("swarm_files", 3) {
        ui.table_setup_column("Action");
        ui.table_setup_column("Path");
        ui.table_setup_column("");
        ui.table_headers_row();
        for (i, row) in ctx.file_rows.iter().enumerate() {
            ui.table_next_row();
            ui.table_next_column(); ui.text(&row.action_label);
            ui.table_next_column();
            let label = format!("{}##file{i}", row.depot_path);
            if row.session_id.is_some() {
                if ui.selectable(label) { *ctx.click_index = Some(i); }
            } else {
                ui.text_disabled(&row.depot_path);
            }
            ui.table_next_column();
            if row.session_id.is_none() { ui.text_disabled("loading…"); }
        }
    }
}
```

- [ ] **Step 4: Declare module**

In `src/app/mod.rs`:

```rust
mod swarm_info_view;
```

- [ ] **Step 5: Build**

Run: `cargo build`
Expected: success.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/app/swarm_info_view.rs src/app/mod.rs
git commit -m "feat(app): Swarm info-tab view (read-only)"
```

---

## Task 10: Loader event drain + tab creation

**Files:**
- Modify: `src/app/mod.rs`
- Modify: `src/session.rs`

- [ ] **Step 1: Add a read-only constructor on SessionStore**

In `src/session.rs`, add after `open_two_way_with`:

```rust
    /// Constructs a read-only 2-way session for Swarm-loaded files.
    /// `Binary`/`Empty` sides are stored as empty strings; the GUI layer
    /// overlays a placeholder message based on the per-tab display state.
    pub fn open_two_way_readonly(
        &self,
        a_text: String,
        b_text: String,
        a_trailing_newline: bool,
        b_trailing_newline: bool,
        engine: Option<String>,
        options: DiffOptions,
    ) -> Result<SessionId, SessionError> {
        let engine = engine.unwrap_or_else(default_engine_name);
        let hunks = recompute_two_way(&engine, &a_text, &b_text, &[], &options)?;
        let id = self.alloc_id();
        let s = DiffSession {
            id, engine, options,
            mode: SessionMode::TwoWay {
                a_text, b_text,
                a_trailing_newline, b_trailing_newline,
                anchors: vec![],
                hunks,
                decisions: HashMap::new(),
            },
            manual_result: None,
            read_only: true,
        };
        self.sessions.lock().unwrap().insert(id, s);
        Ok(id)
    }
```

- [ ] **Step 2: Add SideDisplay enum + Tab field**

In `src/app/mod.rs`:

```rust
#[derive(Clone, Copy, Debug)]
pub enum SideDisplay {
    Normal,
    Added,
    Deleted,
    Binary,
}
```

Add to `struct Tab`:

```rust
    /// For Swarm-loaded 2-way tabs: per-side overlay state (added/deleted/binary).
    side_display: [SideDisplay; 2],
```

Initialize `side_display: [SideDisplay::Normal; 2]` everywhere a Tab is constructed (search for `Tab {`).

- [ ] **Step 3: Implement drain + tab creators**

In `src/app/mod.rs`, find the spot in `frame_ui` after the login-modal block (Task 8 step 4). Add:

```rust
    // Drain Swarm loader events.
    if let Some(handle) = state.swarm_loader.as_ref() {
        while let Ok(ev) = handle.rx.try_recv() {
            use crate::swarm::loader::LoaderEvent as E;
            match ev {
                E::MetaReady(meta) => open_swarm_info_tab(state, meta),
                E::FileTotalKnown(n) => state.swarm_progress = Some((0, n)),
                E::FileReady { entry, left, right } => {
                    open_swarm_file_tab(state, entry, left, right);
                    if let Some((d, t)) = state.swarm_progress.as_mut() { *d += 1; if *d > *t { *d = *t; } }
                }
                E::FileFailed { depot_path, error } => {
                    state.status = format!("Swarm: {depot_path}: {error}");
                    if let Some((d, t)) = state.swarm_progress.as_mut() { *d += 1; if *d > *t { *d = *t; } }
                }
                E::AllDone => { /* leave handle; we just stop polling */ }
            }
        }
    }
```

Add the helpers at module scope:

```rust
fn open_swarm_info_tab(state: &mut AppState, meta: crate::swarm::model::ReviewMeta) {
    let id = state.sessions.next_swarm_info_id();
    let label = match meta.kind {
        crate::swarm::model::TargetKind::Review => format!("Review #{}", meta.id),
        crate::swarm::model::TargetKind::Change => format!("Change #{}", meta.id),
    };
    state.swarm_info_meta.insert(id, meta);
    state.tabs.push(Tab {
        session_id: id,
        label,
        mode: TabMode::SwarmInfo,
        paths: vec![],
        result_path: None,
        path_inputs: vec![],
        side_display: [SideDisplay::Normal; 2],
    });
    state.active = Some(id);
}

fn open_swarm_file_tab(
    state: &mut AppState,
    entry: crate::swarm::model::FileEntry,
    left: crate::swarm::loader::SidePayload,
    right: crate::swarm::loader::SidePayload,
) {
    use crate::swarm::loader::SidePayload;
    let (a_text, a_trailing, disp_a) = match left {
        SidePayload::Text(t, nl) => (t, nl, SideDisplay::Normal),
        SidePayload::Empty => (String::new(), false, SideDisplay::Added),
        SidePayload::Binary => (String::new(), false, SideDisplay::Binary),
    };
    let (b_text, b_trailing, disp_b) = match right {
        SidePayload::Text(t, nl) => (t, nl, SideDisplay::Normal),
        SidePayload::Empty => (String::new(), false, SideDisplay::Deleted),
        SidePayload::Binary => (String::new(), false, SideDisplay::Binary),
    };
    let id = state.sessions.open_two_way_readonly(
        a_text, b_text, a_trailing, b_trailing,
        Some(state.preferences.default_engine.clone()),
        state.preferences.default_options,
    ).expect("open swarm session");
    let label = entry.depot_path
        .rsplit('/')
        .next()
        .unwrap_or(&entry.depot_path)
        .to_string();
    state.tabs.push(Tab {
        session_id: id,
        label,
        mode: TabMode::TwoWay,
        paths: vec![],
        result_path: None,
        path_inputs: vec![],
        side_display: [disp_a, disp_b],
    });
}
```

Add a helper on `SessionStore`:

```rust
impl SessionStore {
    /// Allocates a SessionId without a backing DiffSession — used for the
    /// Swarm info tab which has no diff state.
    pub fn next_swarm_info_id(&self) -> SessionId { self.alloc_id() }
}
```

(Place it in `src/session.rs` near the other impl methods.)

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: success.

- [ ] **Step 5: Commit**

```bash
git add src/session.rs src/app/mod.rs
git commit -m "feat(app): drain Swarm loader; open info + file tabs"
```

---

## Task 11: Render the SwarmInfo tab + browser open

**Files:**
- Modify: `src/app/mod.rs`

- [ ] **Step 1: Dispatch SwarmInfo in the tab-render switch**

In `src/app/mod.rs`, find the place where active-tab rendering happens (search for `TabMode::TwoWay =>` inside `frame_ui` or a sibling fn). Add a third arm:

```rust
            TabMode::SwarmInfo => {
                let meta = state.swarm_info_meta.get(&tab.session_id).cloned();
                if let Some(meta) = meta {
                    let file_rows: Vec<swarm_info_view::InfoFileRow> = state.tabs.iter()
                        .filter(|t| t.mode == TabMode::TwoWay
                            && !t.paths.is_empty() == false // synthetic Swarm tabs have empty paths
                            && state.swarm_info_meta.is_empty() == false)
                        .map(|t| swarm_info_view::InfoFileRow {
                            depot_path: t.label.clone(),
                            action_label: String::new(), // filled below if we tracked it
                            session_id: Some(t.session_id),
                        })
                        .collect();
                    let mut click_index: Option<usize> = None;
                    let mut open_in_browser = false;
                    swarm_info_view::render(ui, swarm_info_view::InfoContext {
                        meta: &meta,
                        progress: state.swarm_progress,
                        file_rows: &file_rows,
                        click_index: &mut click_index,
                        open_in_browser: &mut open_in_browser,
                    });
                    if let Some(i) = click_index {
                        if let Some(row) = file_rows.get(i) {
                            if let Some(sid) = row.session_id {
                                state.active = Some(sid);
                            }
                        }
                    }
                    if open_in_browser {
                        let _ = open::that(&meta.url);
                    }
                }
            }
```

Replace the `file_rows` builder with something that captures the actual file entries by tracking them in a parallel structure. Replace the `AppState` field

```rust
    swarm_info_meta: std::collections::HashMap<SessionId, crate::swarm::model::ReviewMeta>,
```

with:

```rust
    swarm_info_meta: std::collections::HashMap<SessionId, crate::swarm::model::ReviewMeta>,
    /// Per file-tab: the action label shown in the info tab's file list.
    swarm_file_actions: std::collections::HashMap<SessionId, String>,
```

In `open_swarm_file_tab` (Task 10), after computing `id`, also do:

```rust
    let action_label = match &entry.action {
        crate::swarm::model::FileAction::Add => "add".to_string(),
        crate::swarm::model::FileAction::Edit => "edit".to_string(),
        crate::swarm::model::FileAction::Delete => "delete".to_string(),
        crate::swarm::model::FileAction::Rename { from } => format!("rename from {from}"),
        crate::swarm::model::FileAction::Branch => "branch".to_string(),
        crate::swarm::model::FileAction::Integrate => "integrate".to_string(),
    };
    state.swarm_file_actions.insert(id, action_label);
```

And update the file_rows builder to:

```rust
                    let file_rows: Vec<swarm_info_view::InfoFileRow> = state.tabs.iter()
                        .filter(|t| t.mode == TabMode::TwoWay && state.swarm_file_actions.contains_key(&t.session_id))
                        .map(|t| swarm_info_view::InfoFileRow {
                            depot_path: t.label.clone(),
                            action_label: state.swarm_file_actions.get(&t.session_id).cloned().unwrap_or_default(),
                            session_id: Some(t.session_id),
                        })
                        .collect();
```

- [ ] **Step 2: Build**

Run: `cargo build`
Expected: success.

- [ ] **Step 3: Commit**

```bash
git add src/app/mod.rs
git commit -m "feat(app): render SwarmInfo tab, file list + open in browser"
```

---

## Task 12: Read-only 2-way render path

**Files:**
- Modify: `src/app/diff_view/mod.rs`
- Modify: `src/app/mod.rs`

- [ ] **Step 1: Suppress edit widget when session is read-only**

Open `src/app/diff_view/mod.rs`. Find where each pane registers its text editor (search for `input_text_multiline` or similar — there should be a call per side). Wrap the editable-input branch in `if !snap.read_only { ... } else { /* render as static text */ }`. The non-edit path should walk lines and call the existing syntax-painted draw helpers (already used for non-focused rendering — find it as `paint_lines` or similar; reuse).

If the diff view doesn't currently have a separate static path, add one:

```rust
fn render_static_pane(ui: &imgui::Ui, lines: &[&str], theme: &Theme, /* etc */) {
    // Same draw-list painting used elsewhere; intentionally no input_text widget.
    // Mimics paint_lines() but skips selection/cursor handling.
}
```

Then in each pane:

```rust
if snap.read_only {
    render_static_pane(ui, &lines, &state.theme, /* ... */);
} else {
    // existing input_text_multiline branch
}
```

- [ ] **Step 2: Hide Apply A/B overlay buttons**

In the same file, find the hover-overlay code (look for `Apply A` / `Apply B` button labels). Wrap its top with:

```rust
if snap.read_only { return; }
```

Same for the inline per-hunk buttons.

- [ ] **Step 3: Overlay per-side display message**

Where each pane is rendered (the place that picks lines for side A vs B), check the tab's `side_display`:

```rust
match tab.side_display[side_idx] {
    SideDisplay::Normal => { /* render lines as usual */ }
    SideDisplay::Added => render_overlay(ui, "(added in this change)"),
    SideDisplay::Deleted => render_overlay(ui, "(deleted in this change)"),
    SideDisplay::Binary => render_overlay(ui, "(binary file — not diffed)"),
}
```

Helper:

```rust
fn render_overlay(ui: &imgui::Ui, msg: &str) {
    let avail = ui.content_region_avail();
    let cursor = ui.cursor_pos();
    ui.set_cursor_pos([cursor[0] + avail[0] * 0.5 - 100.0, cursor[1] + avail[1] * 0.5 - 8.0]);
    ui.text_disabled(msg);
}
```

- [ ] **Step 4: Skip undo stack for read-only sessions**

In `src/app/mod.rs`, find every place that creates an `undo_stacks` entry (search for `undo_stacks.insert`). Guard with:

```rust
if !session_is_read_only(state, id) {
    state.undo_stacks.insert(id, undo_stack::Stack::new());
}
```

Add:

```rust
fn session_is_read_only(state: &AppState, id: SessionId) -> bool {
    state.sessions.snapshot(id).map(|s| s.read_only).unwrap_or(false)
}
```

- [ ] **Step 5: Disable Save / Save As / Save File A / Save File B / Undo / Redo menu items for read-only tabs**

Find the File menu and Edit menu construction in `src/app/mod.rs` (search for `menu("File")` / `menu("Edit")`). For each `MenuItem::new("Save…")`-style entry, add `.enabled(!ro)` where `ro = state.active.map(|id| session_is_read_only(state, id)).unwrap_or(false)`.

- [ ] **Step 6: Build + smoke test**

Run: `cargo build && cargo test --no-default-features --lib`
Expected: build succeeds; core tests still pass.

- [ ] **Step 7: Commit**

```bash
git add src/app/diff_view/mod.rs src/app/mod.rs
git commit -m "feat(diff-view): read-only render + per-side overlay messages"
```

---

## Task 13: End-to-end manual smoke test

**Files:** none (manual)

- [ ] **Step 1: Build a release binary**

Run: `cargo build --release`
Expected: success.

- [ ] **Step 2: Test bad URL**

Run: `./target/release/diffie "https://example.com/not-a-review"`
Expected: usage printed to stderr, exit 2.

- [ ] **Step 3: Test real Swarm URL (requires a live host)**

Run: `./target/release/diffie "https://<your-swarm>/reviews/<id>"`
Expected:
- Login modal appears (username prefilled if previously stored).
- After login, info tab opens with title, description, file list, progress bar.
- File tabs stream in.
- Adds show "(added)" on left side; deletes show "(deleted)" on right; binaries show "(binary)".
- All file tabs reject edits (typing into a pane does nothing).
- File menu: Save / Save As disabled while active tab is Swarm-loaded.
- "Open in browser" launches default browser to the review URL.
- Relaunch the same URL: no password prompt (cached ticket + probe succeeded).
- Manually invalidate ticket in keychain ⇒ next launch prompts again.

- [ ] **Step 4: Test changelist URL**

Run: `./target/release/diffie "https://<your-swarm>/changes/<id>"`
Expected: info tab labeled "Change #N", no reviewers section, file list and tabs behave as above.

- [ ] **Step 5: Done — no commit needed if everything passed**

If any issues found, file as follow-up tasks before declaring complete.

---

## Self-Review Notes

**Spec coverage** — all sections of `2026-05-20-swarm-url-launch-design.md` map to tasks above:
- URL parsing → Task 1
- Read-only sessions → Tasks 2, 10, 12
- Models → Task 3
- HTTP client (`SwarmApi` trait + ureq impl) → Task 4
- Loader → Task 5
- Credentials → Task 6
- CLI + `InitialOpen::Swarm` → Task 7
- Login modal → Task 8
- Info-tab view → Task 9
- Loader event drain + tab creation → Tasks 10, 11
- Read-only render path + overlays + menu guards → Task 12
- Manual smoke test → Task 13

**Known soft spots** that the implementing engineer may need to adjust:
- `RawFile::into_entry` infers `rev_pre = rev_post - 1`. If real Swarm responses include explicit pre/post revisions, prefer those (look for `revBefore` / `rev` pairs and adjust).
- The Swarm `/api/v9/login` response shape may vary; the implementation handles two common forms (`{ticket}` and `{user:{ticket}}`).
- `imgui-rs` API method names (e.g. `modal_popup_config`, `selectable`) follow the version pinned in `Cargo.toml` (0.12). If the snippets don't compile verbatim, use the analogous API; the structure stays the same.
- File-content endpoint `/api/v10/files/{path}?rev=N&fields=content` may need `Accept: application/json` and the response unwrapped from `data.files[0].content` (base64). The implementing engineer should verify against the actual Swarm version and adjust `get_file_content` decoding accordingly — keep the `SwarmApi` signature stable.
