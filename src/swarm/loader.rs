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
