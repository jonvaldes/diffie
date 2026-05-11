import { invoke } from "@tauri-apps/api/core";
import type {
  HunkDecision,
  Resolution,
  SessionView,
} from "./types";

export const api = {
  openTwoWay: (path_a: string, path_b: string, engine?: string) =>
    invoke<SessionView>("open_two_way", { pathA: path_a, pathB: path_b, engine: engine ?? null }),

  openThreeWay: (path_base: string, path_local: string, path_remote: string, engine?: string) =>
    invoke<SessionView>("open_three_way", {
      pathBase: path_base, pathLocal: path_local, pathRemote: path_remote,
      engine: engine ?? null,
    }),

  getSession: (sessionId: number) => invoke<SessionView>("get_session", { sessionId }),

  addTwoWayAnchor: (sessionId: number, a: number, b: number) =>
    invoke<SessionView>("add_two_way_anchor", { sessionId, a, b }),

  addThreeWayAnchor: (sessionId: number, base: number, local: number, remote: number) =>
    invoke<SessionView>("add_three_way_anchor", { sessionId, base, local, remote }),

  removeAnchor: (sessionId: number, index: number) =>
    invoke<SessionView>("remove_anchor", { sessionId, index }),

  setEngine: (sessionId: number, engine: string) =>
    invoke<SessionView>("set_engine", { sessionId, engine }),

  setTwoWayDecision: (sessionId: number, hunkId: number, decision: HunkDecision) =>
    invoke<void>("set_two_way_decision", { sessionId, hunkId, decision }),

  setThreeWayResolution: (sessionId: number, hunkId: number, resolution: Resolution) =>
    invoke<void>("set_three_way_resolution", { sessionId, hunkId, resolution }),

  updateResult: (sessionId: number, text: string) =>
    invoke<void>("update_result", { sessionId, text }),

  computeResult: (sessionId: number) =>
    invoke<string>("compute_result", { sessionId }),

  saveResult: (sessionId: number, path: string) =>
    invoke<void>("save_result", { sessionId, path }),

  availableEngines: () => invoke<string[]>("available_engines"),
};
