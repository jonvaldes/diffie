//! Tauri command handlers. Thin wrappers over `SessionStore`.

use serde::Serialize;
use tauri::State;

use crate::diff::{Anchor, Hunk};
use crate::io as fileio;
use crate::merge::{MergeAnchor, MergeHunk, Resolution};
use crate::session::{
    available_engines as engines_list, DiffSession, HunkDecision, SessionError, SessionId,
    SessionMode, SessionStore,
};

#[derive(Serialize)]
pub struct TwoWayView {
    pub session_id: SessionId,
    pub engine: String,
    pub a_lines: Vec<String>,
    pub b_lines: Vec<String>,
    pub anchors: Vec<Anchor>,
    pub hunks: Vec<Hunk>,
    pub manual_result: Option<String>,
}

#[derive(Serialize)]
pub struct ThreeWayView {
    pub session_id: SessionId,
    pub engine: String,
    pub base_lines: Vec<String>,
    pub local_lines: Vec<String>,
    pub remote_lines: Vec<String>,
    pub anchors: Vec<MergeAnchor>,
    pub hunks: Vec<MergeHunk>,
    pub manual_result: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum SessionView {
    TwoWay(TwoWayView),
    ThreeWay(ThreeWayView),
}

fn to_view(s: DiffSession) -> SessionView {
    match s.mode {
        SessionMode::TwoWay { a_lines, b_lines, anchors, hunks, .. } => {
            SessionView::TwoWay(TwoWayView {
                session_id: s.id,
                engine: s.engine,
                a_lines, b_lines, anchors, hunks,
                manual_result: s.manual_result,
            })
        }
        SessionMode::ThreeWay { base_lines, local_lines, remote_lines, anchors, hunks, .. } => {
            SessionView::ThreeWay(ThreeWayView {
                session_id: s.id,
                engine: s.engine,
                base_lines, local_lines, remote_lines, anchors, hunks,
                manual_result: s.manual_result,
            })
        }
    }
}

fn err(e: impl std::fmt::Display) -> String { format!("{e}") }

#[tauri::command]
pub fn open_two_way(
    path_a: String,
    path_b: String,
    engine: Option<String>,
    store: State<'_, SessionStore>,
) -> Result<SessionView, String> {
    let a = fileio::read_text(&path_a).map_err(err)?;
    let b = fileio::read_text(&path_b).map_err(err)?;
    let id = store.open_two_way(&a, &b, engine).map_err(err)?;
    let snap = store.snapshot(id).map_err(err)?;
    Ok(to_view(snap))
}

#[tauri::command]
pub fn open_three_way(
    path_base: String,
    path_local: String,
    path_remote: String,
    engine: Option<String>,
    store: State<'_, SessionStore>,
) -> Result<SessionView, String> {
    let base = fileio::read_text(&path_base).map_err(err)?;
    let local = fileio::read_text(&path_local).map_err(err)?;
    let remote = fileio::read_text(&path_remote).map_err(err)?;
    let id = store
        .open_three_way(&base, &local, &remote, engine)
        .map_err(err)?;
    let snap = store.snapshot(id).map_err(err)?;
    Ok(to_view(snap))
}

#[tauri::command]
pub fn get_session(session_id: SessionId, store: State<'_, SessionStore>) -> Result<SessionView, String> {
    Ok(to_view(store.snapshot(session_id).map_err(err)?))
}

#[tauri::command]
pub fn add_two_way_anchor(
    session_id: SessionId,
    a: u32,
    b: u32,
    store: State<'_, SessionStore>,
) -> Result<SessionView, String> {
    store.add_anchor_two_way(session_id, Anchor { a, b }).map_err(err)?;
    Ok(to_view(store.snapshot(session_id).map_err(err)?))
}

#[tauri::command]
pub fn add_three_way_anchor(
    session_id: SessionId,
    base: u32,
    local: u32,
    remote: u32,
    store: State<'_, SessionStore>,
) -> Result<SessionView, String> {
    store
        .add_anchor_three_way(session_id, MergeAnchor { base, local, remote })
        .map_err(err)?;
    Ok(to_view(store.snapshot(session_id).map_err(err)?))
}

#[tauri::command]
pub fn remove_anchor(
    session_id: SessionId,
    index: usize,
    store: State<'_, SessionStore>,
) -> Result<SessionView, String> {
    store.remove_anchor(session_id, index).map_err(err)?;
    Ok(to_view(store.snapshot(session_id).map_err(err)?))
}

#[tauri::command]
pub fn set_engine(
    session_id: SessionId,
    engine: String,
    store: State<'_, SessionStore>,
) -> Result<SessionView, String> {
    store.set_engine(session_id, engine).map_err(err)?;
    Ok(to_view(store.snapshot(session_id).map_err(err)?))
}

#[tauri::command]
pub fn set_two_way_decision(
    session_id: SessionId,
    hunk_id: u32,
    decision: HunkDecision,
    store: State<'_, SessionStore>,
) -> Result<(), String> {
    store
        .set_two_way_decision(session_id, hunk_id, decision)
        .map_err(err)
}

#[tauri::command]
pub fn set_three_way_resolution(
    session_id: SessionId,
    hunk_id: u32,
    resolution: Resolution,
    store: State<'_, SessionStore>,
) -> Result<(), String> {
    store
        .set_three_way_resolution(session_id, hunk_id, resolution)
        .map_err(err)
}

#[tauri::command]
pub fn update_result(
    session_id: SessionId,
    text: String,
    store: State<'_, SessionStore>,
) -> Result<(), String> {
    store.update_manual_result(session_id, text).map_err(err)
}

#[tauri::command]
pub fn compute_result(
    session_id: SessionId,
    store: State<'_, SessionStore>,
) -> Result<String, String> {
    store.compute_result(session_id).map_err(err)
}

#[tauri::command]
pub fn save_result(
    session_id: SessionId,
    path: String,
    store: State<'_, SessionStore>,
) -> Result<(), String> {
    let text = store.compute_result(session_id).map_err(err)?;
    fileio::write_text(&path, &text).map_err(err)
}

#[tauri::command]
pub fn available_engines() -> Vec<String> {
    engines_list()
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .manage(SessionStore::new())
        .invoke_handler(tauri::generate_handler![
            open_two_way,
            open_three_way,
            get_session,
            add_two_way_anchor,
            add_three_way_anchor,
            remove_anchor,
            set_engine,
            set_two_way_decision,
            set_three_way_resolution,
            update_result,
            compute_result,
            save_result,
            available_engines,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// Expose SessionError for completeness (currently unused publicly).
#[allow(dead_code)]
fn _ensure_err_type() -> Option<SessionError> { None }
