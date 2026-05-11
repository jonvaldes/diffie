// Mirrors of Rust DTOs (kept hand-written; small enough). If these grow,
// switch to a codegen tool (ts-rs / specta).

export type LineNo = number;

export type DiffOp =
  | { kind: "equal"; a: LineNo; b: LineNo; text: string }
  | { kind: "delete"; a: LineNo; text: string }
  | { kind: "insert"; b: LineNo; text: string };

export interface Hunk {
  id: number;
  a_range: [LineNo, LineNo];
  b_range: [LineNo, LineNo];
  ops: DiffOp[];
}

export interface Anchor { a: LineNo; b: LineNo }
export interface MergeAnchor { base: LineNo; local: LineNo; remote: LineNo }

export type MergeHunk =
  | { kind: "stable"; id: number; text: string[] }
  | { kind: "local_only"; id: number; base: string[]; local: string[] }
  | { kind: "remote_only"; id: number; base: string[]; remote: string[] }
  | { kind: "conflict"; id: number; base: string[]; local: string[]; remote: string[] };

export type Resolution =
  | { kind: "local" }
  | { kind: "remote" }
  | { kind: "base" }
  | { kind: "custom"; text: string[] };

export type HunkDecision =
  | { kind: "accept_a" }
  | { kind: "accept_b" }
  | { kind: "both" }
  | { kind: "neither" }
  | { kind: "custom"; text: string[] }
  | { kind: "per_line"; keep: boolean[] };

export interface TwoWayView {
  mode: "two_way";
  session_id: number;
  engine: string;
  a_lines: string[];
  b_lines: string[];
  anchors: Anchor[];
  hunks: Hunk[];
  manual_result: string | null;
}

export interface ThreeWayView {
  mode: "three_way";
  session_id: number;
  engine: string;
  base_lines: string[];
  local_lines: string[];
  remote_lines: string[];
  anchors: MergeAnchor[];
  hunks: MergeHunk[];
  manual_result: string | null;
}

export type SessionView = TwoWayView | ThreeWayView;
