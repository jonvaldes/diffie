# Swarm URL launch — design

Launch Diffie with a Helix Swarm URL on the command line. The app loads the
referenced review or changelist, opens one read-only 2-way diff tab per file,
and adds an info tab summarizing the review/CL.

## Goals

- `diffie <swarm-url>` opens a review or changelist as read-only tabs.
- Each affected file becomes a 2-way diff tab (pre-change vs post-change).
- An info tab shows metadata (description, author, dates, state, file list,
  reviewers/votes) and a button to open the page in the system browser.
- Swarm-loaded sessions are read-only: editing, saves, and Apply A/B are
  disabled.
- Username + ticket are persisted per Swarm host in the OS keychain so the
  user only enters their password the first time.

## Non-goals

- Editing or saving files loaded from Swarm.
- Posting comments, voting, or any state-changing API calls.
- Opening multiple Swarm URLs in one launch.
- Picking arbitrary revision pairs (always pre vs post of the CL).
- Mixing Swarm tabs with local file tabs in the same launch.

## CLI

`main.rs` accepts a single argument and, if it parses as a Swarm URL, opens
in Swarm mode. The existing 2-arg and 4-arg forms are unchanged.

Accepted URL shapes:

- `https://<host>/reviews/<id>` (with optional `/files`, `/` trailing, or
  `#fragment` — all stripped)
- `https://<host>/changes/<id>` (same trailing-bits handling)

Anything else: print usage + exit 2.

## URL parsing — `src/swarm/url.rs`

```rust
pub enum SwarmTarget { Review(u64), Change(u64) }
pub struct SwarmUrl { pub host: String, pub target: SwarmTarget }
pub fn parse(s: &str) -> Result<SwarmUrl, ParseError>;
```

`host` is `scheme://authority` (e.g. `https://swarm.example.com`), used both
for HTTP requests and as the keychain key. Pure function; full unit tests
covering each accepted/rejected shape.

## Auth

### Credentials

Stored via the `keyring` crate. Service name `"diffie-swarm"`. Two entries
per host:

- account `"{host}:user"` → username
- account `"{host}:ticket"` → P4 ticket

Helper module `src/app/swarm_creds.rs`:

```rust
pub fn load(host: &str) -> Option<(String, String)>;
pub fn store(host: &str, user: &str, ticket: &str);
pub fn clear(host: &str);
```

### Login flow

1. On launch with a Swarm URL, App constructs `SwarmAuth::Pending { url }`
   and shows a modal login dialog *before* any tabs are created.
2. Modal fields: host (read-only, derived from URL), username (prefilled
   from keychain if present), password (masked), Login / Cancel buttons.
3. Submit dispatches a background `client::login(host, user, password)`.
   While pending, the modal shows a spinner.
4. On success, store `(user, ticket)` in the keychain and proceed to
   loader startup.
5. On failure (401, network, etc.), show inline error; let user retry.
6. Cancel exits the app (there is no other session to drop back to).

If the keychain already has a valid-looking `(user, ticket)`, we still
show the modal for one frame to attempt a no-op authenticated request
(GET `/api/v9/projects?max=1`); on 401 fall through to the password
prompt; on success skip the modal entirely. (Quick "is ticket alive"
probe.)

## HTTP client — `src/swarm/client.rs`

Uses `ureq` (blocking, rustls). One `ureq::Agent` per `Client`. All
authenticated requests use HTTP Basic Auth `user:ticket`.

```rust
pub struct Client { http: ureq::Agent, host: String, ticket: Option<String> }

impl Client {
    pub fn login(host: &str, user: &str, password: &str) -> Result<String, Error>;
    pub fn get_review(&self, id: u64) -> Result<Review, Error>;
    pub fn get_change(&self, id: u64) -> Result<Change, Error>;
    pub fn get_file_content(&self, depot_path: &str, rev: u32) -> Result<Vec<u8>, Error>;
}

pub enum Error { Network(ureq::Error), Auth, NotFound, Decode(String) }
```

Endpoints (Swarm v9/v10):

- `POST {host}/api/v9/login` body `{user,password}` → `{user, ticket}`.
- `GET {host}/api/v9/reviews/{id}`.
- `GET {host}/api/v9/changes/{id}` (file list comes via the embedded
  `changes/{id}/files` resource if not in the change response).
- `GET {host}/api/v10/files?path={depot_path}%23{rev}&fields=content`
  returns raw bytes. UTF-8 fallback via `encoding_rs` (same path as
  `io::read_text`).

Response decoding uses `serde_json` against minimal DTOs in `model.rs`.

## Models — `src/swarm/model.rs`

```rust
pub struct ReviewMeta {
    pub id: u64,
    pub kind: TargetKind,   // Review or Change
    pub description: String,
    pub author: String,
    pub state: String,
    pub created: DateTime,
    pub updated: Option<DateTime>,
    pub participants: Vec<Participant>, // reviews only
    pub url: String,        // canonical web URL for "open in browser"
}

pub struct FileEntry {
    pub depot_path: String,
    pub action: FileAction,
    pub rev_pre: Option<u32>,
    pub rev_post: Option<u32>,
    pub is_text: bool,
}

pub enum FileAction { Add, Edit, Delete, Rename { from: String }, Branch, Integrate, Binary }
```

`rev_pre = None` for `Add`; `rev_post = None` for `Delete`.

## Background loader — `src/swarm/loader.rs`

Spawns one orchestrator thread that:

1. Fetches review/change metadata. Posts `LoaderEvent::MetaReady`.
2. Resolves the file list. Posts `LoaderEvent::FileTotalKnown(n)`.
3. Fans out up to 4 worker threads to fetch each file's pre/post content
   in parallel. Each completed file posts `LoaderEvent::FileReady`.
4. Posts `LoaderEvent::AllDone`.

```rust
pub enum LoaderEvent {
    MetaReady(ReviewMeta),
    FileTotalKnown(usize),
    FileReady { entry: FileEntry, left: SidePayload, right: SidePayload },
    FileFailed { depot_path: String, error: String },
    AllDone,
}

pub enum SidePayload { Text(String, bool /*trailing_newline*/), Binary, Empty /*add/delete*/ }
```

Communication via `std::sync::mpsc`. `AppState` owns the `Receiver` and
drains it once per frame in `frame_ui`.

## App integration

### main.rs

```rust
match args.len() {
    0 => None,
    1 => match swarm::url::parse(&args[0]) {
        Ok(u) => Some(InitialOpen::Swarm(u)),
        Err(_) => { print_usage(...); exit(2); }
    },
    2 => Some(InitialOpen::TwoWay { .. }),     // unchanged
    4 => Some(InitialOpen::ThreeWay { .. }),   // unchanged
    _ => { print_usage(...); exit(2); }
}
```

### AppState additions

- `swarm_auth: Option<SwarmAuth>` — drives the login modal state machine.
- `swarm_loader: Option<LoaderHandle>` — holds the mpsc receiver and
  joinhandle; cleared on `AllDone` (the join is detached).
- `swarm_progress: Option<(done: usize, total: usize)>` — surfaced in the
  info tab.

### Tabs

New `TabMode::SwarmInfo` variant. `Tab` for the info tab carries
`ReviewMeta`. File tabs are normal `TabMode::TwoWay` but their session
has `read_only = true` (see below).

## Read-only sessions

`DiffSession` gains `pub read_only: bool` (defaults false). The single
edit entry point `SessionStore::set_side_text` returns immediately if
`read_only`.

`SessionStore::new_swarm_two_way(name, a: SidePayload, b: SidePayload,
engine: &str, opts: DiffOptions) -> SessionId` constructs the session
with the flag set, mapping `Binary`/`Empty` payloads to empty strings
plus a stored `display_state` (see below).

### UI guards in the 2-way view

- Skip the imgui text-input widget; render text using the existing
  syntax-painted draw-list path (already used for non-focused rows).
- Don't draw Apply-A/B hover/inline buttons.
- Don't register undo entries; skip stack creation for read-only sessions.

### Menu/shortcut guards

- File > Save, Save As, Save File A/B: disabled when active session is
  read-only.
- Edit > Undo/Redo: disabled.

### Per-side display state

For deletes/adds/binaries we still want the user to see *something*. The
session text is empty for those sides; the UI checks a per-side
`SideDisplay` enum (carried on the tab, not the session) and overlays
"(added)" / "(deleted)" / "(binary file)" centered in the pane when set.

## Info tab — `src/app/swarm_info_view.rs`

Layout (top-to-bottom):

1. Header row: `#1234 — Review` / `#5678 — Change`, state badge,
   author, created date.
2. Progress bar (`done / total files loaded`) — replaced by
   `"Loaded N files"` text when complete.
3. Description (read-only `input_text_multiline` with `ReadOnly` flag,
   sized to content up to a max).
4. Reviewers + votes table (reviews only).
5. File list table: depot path, action chip; clicking a row selects that
   file's tab.
6. "Open in browser" button → `open::that(meta.url)`.

The progress bar reads `AppState::swarm_progress`. File list rows for
files still loading are shown disabled with a spinner glyph.

## Per-frame event drain

In `frame_ui`, before rendering tabs:

```rust
if let Some(loader) = &app.swarm_loader {
    while let Ok(event) = loader.rx.try_recv() {
        match event {
            MetaReady(meta) => app.open_swarm_info_tab(meta),
            FileTotalKnown(n) => app.swarm_progress = Some((0, n)),
            FileReady { entry, left, right } => app.open_swarm_file_tab(entry, left, right),
            FileFailed { depot_path, error } => app.status = format!("failed: {depot_path}: {error}"),
            AllDone => app.swarm_loader = None,
        }
    }
}
```

## Error handling

- URL parse failure → usage + exit 2.
- Login network failure → modal shows error, retry available.
- 401 on any subsequent request → drop ticket, clear keychain `ticket`
  entry, re-show login modal preserving the username.
- Per-file fetch failure → keep info tab updating; that file's tab is
  still created but shows the error message in place of content (left
  and right) and is marked read-only.

## Dependencies (Cargo.toml)

All added to the `gui` feature, since none are needed by the core lib:

- `ureq` (with `tls` / rustls feature)
- `keyring`
- `url` (parsing helper)
- `open` (system browser)
- `chrono` *(optional — if we want pretty date formatting; otherwise
  format Swarm's epoch ints by hand)*

## Testing

Core (`--no-default-features --lib`):

- `swarm::url::parse` — table of accepted/rejected URLs.
- `swarm::model` serde round-trips against captured JSON fixtures
  (sample review, sample change, sample files list, error responses).

GUI:

- `session::set_side_text` no-ops when `read_only=true` (existing tests
  unchanged when `read_only` defaults to false).
- A small integration test for the loader using a mock `Client` trait
  (extract the HTTP calls behind a `trait SwarmApi` for this).

Manual checklist:

- Launch with `/reviews/N` and `/changes/N` URLs (real Swarm + offline
  recordings).
- Wrong password → error in modal, retry succeeds.
- Stale ticket → seamless re-login on first 401.
- Add / Delete / Rename / Binary files render correctly.
- Edit attempts on Swarm-loaded tabs are blocked; menus disabled.
- Open-in-browser launches the system browser.

## File map (new + modified)

New:

- `src/swarm/mod.rs`
- `src/swarm/url.rs`
- `src/swarm/model.rs`
- `src/swarm/client.rs`
- `src/swarm/loader.rs`
- `src/app/swarm_login.rs`
- `src/app/swarm_info_view.rs`
- `src/app/swarm_creds.rs`

Modified:

- `Cargo.toml` (deps under `gui` feature)
- `src/lib.rs` (re-export `swarm` module)
- `src/main.rs` (1-arg URL routing)
- `src/app/mod.rs` (`InitialOpen::Swarm`, `TabMode::SwarmInfo`, loader
  drain, menu guards)
- `src/session.rs` (`read_only` field; `set_side_text` guard; helper
  constructor)
- `src/app/diff_view/mod.rs` (read-only render path, hide overlays)
- `src/app/undo_stack.rs` (skip stack for read-only sessions)
