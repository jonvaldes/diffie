//! Dual-monitor mode: discover adjacent monitor pairs, pick a default
//! pair near a given anchor rect, and match a persisted signature back
//! to a live pair.
//!
//! Pure data here — no winit types — so the adjacency/matching logic
//! can be unit-tested without a real window.

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MonitorRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl MonitorRect {
    pub fn right(&self) -> i32 {
        self.x + self.w as i32
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h as i32
    }
    pub fn center(&self) -> (i32, i32) {
        (
            self.x + (self.w as i32) / 2,
            self.y + (self.h as i32) / 2,
        )
    }
}

/// Two monitors that share an edge (within `EDGE_TOLERANCE` px) and overlap
/// on the perpendicular axis. Ordered so `.0` is the top/left one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorPair(pub MonitorRect, pub MonitorRect);

pub const EDGE_TOLERANCE: i32 = 4;

pub fn are_adjacent(a: &MonitorRect, b: &MonitorRect) -> bool {
    let touch_h = (a.right() - b.x).abs() <= EDGE_TOLERANCE
        || (b.right() - a.x).abs() <= EDGE_TOLERANCE;
    let overlap_v = a.y < b.bottom() && b.y < a.bottom();
    if touch_h && overlap_v {
        return true;
    }
    let touch_v = (a.bottom() - b.y).abs() <= EDGE_TOLERANCE
        || (b.bottom() - a.y).abs() <= EDGE_TOLERANCE;
    let overlap_h = a.x < b.right() && b.x < a.right();
    touch_v && overlap_h
}

pub fn adjacent_pairs(monitors: &[MonitorRect]) -> Vec<MonitorPair> {
    let mut out = Vec::new();
    for i in 0..monitors.len() {
        for j in (i + 1)..monitors.len() {
            let a = monitors[i];
            let b = monitors[j];
            if !are_adjacent(&a, &b) {
                continue;
            }
            let pair = if (a.y, a.x) <= (b.y, b.x) {
                MonitorPair(a, b)
            } else {
                MonitorPair(b, a)
            };
            out.push(pair);
        }
    }
    out
}

pub fn union_rect(pair: &MonitorPair) -> MonitorRect {
    let x = pair.0.x.min(pair.1.x);
    let y = pair.0.y.min(pair.1.y);
    let right = pair.0.right().max(pair.1.right());
    let bottom = pair.0.bottom().max(pair.1.bottom());
    MonitorRect {
        x,
        y,
        w: (right - x) as u32,
        h: (bottom - y) as u32,
    }
}

pub fn pick_pair_near(
    monitors: &[MonitorRect],
    anchor_center: (i32, i32),
) -> Option<MonitorPair> {
    let pairs = adjacent_pairs(monitors);
    if pairs.is_empty() {
        return None;
    }
    if let Some(p) = pairs.iter().find(|p| {
        let u = union_rect(p);
        (u.x..u.right()).contains(&anchor_center.0)
            && (u.y..u.bottom()).contains(&anchor_center.1)
    }) {
        return Some(*p);
    }
    pairs.into_iter().min_by_key(|p| {
        let u = union_rect(p);
        let (cx, cy) = u.center();
        let dx = (cx - anchor_center.0) as i64;
        let dy = (cy - anchor_center.1) as i64;
        dx * dx + dy * dy
    })
}

pub fn match_saved_pair(
    monitors: &[MonitorRect],
    saved: &[MonitorRect; 2],
) -> Option<MonitorPair> {
    fn approx(a: &MonitorRect, b: &MonitorRect) -> bool {
        (a.x - b.x).abs() <= EDGE_TOLERANCE
            && (a.y - b.y).abs() <= EDGE_TOLERANCE
            && (a.w as i32 - b.w as i32).abs() <= EDGE_TOLERANCE
            && (a.h as i32 - b.h as i32).abs() <= EDGE_TOLERANCE
    }
    for p in adjacent_pairs(monitors) {
        let s0 = &saved[0];
        let s1 = &saved[1];
        if (approx(&p.0, s0) && approx(&p.1, s1))
            || (approx(&p.0, s1) && approx(&p.1, s0))
        {
            return Some(p);
        }
    }
    None
}

pub fn pair_label(pair: &MonitorPair) -> String {
    let u = union_rect(pair);
    format!(
        "({}, {}) + ({}, {})  —  {}×{}",
        pair.0.x, pair.0.y, pair.1.x, pair.1.y, u.w, u.h,
    )
}

use std::sync::Arc;
use winit::window::Window;

/// Snapshot of window state captured at the moment dual-monitor mode is
/// entered, so we can restore it on exit. Re-read live from winit rather
/// than reusing `AppPreferences::window` because the user may have moved
/// or resized the window since the last placement save.
#[derive(Debug, Clone)]
pub struct PriorWindowState {
    pub outer_position: (i32, i32),
    pub inner_size: (u32, u32),
    pub maximized: bool,
    pub decorations: bool,
}

/// Active dual-monitor session. Held on the `Gpu` while the mode is on.
#[derive(Debug, Clone)]
pub struct ActiveDualMonitor {
    pub pair: MonitorPair,
    pub prior: PriorWindowState,
}

/// Read the live monitors from the window and convert to plain rects.
pub fn monitors_from_window(window: &Window) -> Vec<MonitorRect> {
    window
        .available_monitors()
        .map(|m| {
            let pos = m.position();
            let size = m.size();
            MonitorRect {
                x: pos.x,
                y: pos.y,
                w: size.width,
                h: size.height,
            }
        })
        .collect()
}

/// Capture the window's current placement so we can restore it on exit.
/// winit doesn't expose a getter for `is_decorated`, so we assume the
/// default (decorated = true) — Diffie never toggles decorations except
/// via this module, so the assumption holds.
pub fn snapshot_prior(window: &Window) -> PriorWindowState {
    let pos = window
        .outer_position()
        .map(|p| (p.x, p.y))
        .unwrap_or((0, 0));
    let size = window.inner_size();
    PriorWindowState {
        outer_position: pos,
        inner_size: (size.width, size.height),
        maximized: window.is_maximized(),
        decorations: true,
    }
}

/// Move + resize the window to span the pair, borderless.
pub fn enter(window: &Arc<Window>, pair: MonitorPair) -> ActiveDualMonitor {
    let prior = snapshot_prior(window);
    let u = union_rect(&pair);
    // Order matters: un-maximize first (set_outer_position is a no-op
    // while maximized on Windows), drop decorations, then move + size.
    if prior.maximized {
        window.set_maximized(false);
    }
    window.set_decorations(false);
    window.set_outer_position(winit::dpi::PhysicalPosition::new(u.x, u.y));
    let _ = window
        .request_inner_size(winit::dpi::PhysicalSize::new(u.w, u.h));
    ActiveDualMonitor { pair, prior }
}

/// Restore decorations + the captured prior placement.
pub fn exit(window: &Arc<Window>, active: &ActiveDualMonitor) {
    window.set_decorations(active.prior.decorations);
    window.set_outer_position(winit::dpi::PhysicalPosition::new(
        active.prior.outer_position.0,
        active.prior.outer_position.1,
    ));
    let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(
        active.prior.inner_size.0,
        active.prior.inner_size.1,
    ));
    if active.prior.maximized {
        window.set_maximized(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(x: i32, y: i32, w: u32, h: u32) -> MonitorRect {
        MonitorRect { x, y, w, h }
    }

    #[test]
    fn adjacent_horizontal_pair_is_detected() {
        let a = m(0, 0, 1920, 1080);
        let b = m(1920, 0, 1920, 1080);
        assert!(are_adjacent(&a, &b));
    }

    #[test]
    fn non_adjacent_pair_rejected() {
        let a = m(0, 0, 1920, 1080);
        let b = m(3000, 0, 1920, 1080);
        assert!(!are_adjacent(&a, &b));
    }

    #[test]
    fn three_monitor_layout_lists_two_adjacent_pairs() {
        let mons = vec![
            m(0, 0, 1920, 1080),
            m(1920, 0, 1920, 1080),
            m(3840, 0, 1920, 1080),
        ];
        let pairs = adjacent_pairs(&mons);
        assert_eq!(pairs.len(), 2);
    }

    #[test]
    fn union_rect_spans_both_monitors() {
        let p = MonitorPair(m(0, 0, 1920, 1080), m(1920, 0, 1920, 1080));
        assert_eq!(union_rect(&p), m(0, 0, 3840, 1080));
    }

    #[test]
    fn pick_pair_near_prefers_containing_pair() {
        let mons = vec![
            m(0, 0, 1920, 1080),
            m(1920, 0, 1920, 1080),
            m(3840, 0, 1920, 1080),
        ];
        let p = pick_pair_near(&mons, (4500, 500)).expect("a pair");
        assert_eq!(p, MonitorPair(m(1920, 0, 1920, 1080), m(3840, 0, 1920, 1080)));
    }

    #[test]
    fn match_saved_pair_tolerates_small_drift() {
        let mons = vec![
            m(0, 0, 1920, 1080),
            m(1920, 0, 1920, 1080),
        ];
        let saved = [m(1, 0, 1920, 1080), m(1921, 0, 1920, 1080)];
        assert!(match_saved_pair(&mons, &saved).is_some());
    }

    #[test]
    fn match_saved_pair_fails_when_signatures_differ() {
        let mons = vec![
            m(0, 0, 1920, 1080),
            m(1920, 0, 1920, 1080),
        ];
        let saved = [m(0, 0, 2560, 1440), m(2560, 0, 2560, 1440)];
        assert!(match_saved_pair(&mons, &saved).is_none());
    }
}
