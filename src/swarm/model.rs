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
