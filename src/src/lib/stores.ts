import { writable } from "svelte/store";
import type { SessionView } from "./types";

export const session = writable<SessionView | null>(null);
export const engines = writable<string[]>([]);
export const status = writable<string>("Open files to begin.");

export interface Tab {
  sessionId: number;
  label: string;
  mode: "two_way" | "three_way";
}
export const tabs = writable<Tab[]>([]);
export const activeTabId = writable<number | null>(null);

/// A pending click on a pane line — used to build anchors via two-click UX.
export interface PendingAnchorPick {
  side: "a" | "b" | "base" | "local" | "remote";
  line: number;
}
export const pendingAnchor = writable<PendingAnchorPick[]>([]);
