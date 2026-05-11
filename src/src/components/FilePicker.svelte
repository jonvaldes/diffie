<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";
  import { api } from "../lib/tauri";
  import { session, status, tabs, activeTabId } from "../lib/stores";
  import type { SessionView } from "../lib/types";

  async function pick(): Promise<string | null> {
    const r = await open({ multiple: false, directory: false });
    return typeof r === "string" ? r : null;
  }

  function basename(p: string): string {
    const i = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
    return i >= 0 ? p.slice(i + 1) : p;
  }

  function registerTab(view: SessionView, label: string) {
    tabs.update(list => {
      if (list.some(t => t.sessionId === view.session_id)) return list;
      return [...list, { sessionId: view.session_id, label, mode: view.mode }];
    });
    activeTabId.set(view.session_id);
    session.set(view);
  }

  async function openTwo() {
    const a = await pick();
    if (!a) return;
    const b = await pick();
    if (!b) return;
    try {
      const view = await api.openTwoWay(a, b);
      registerTab(view, `${basename(a)} ↔ ${basename(b)}`);
      status.set(`2-way: ${a} ↔ ${b}`);
    } catch (e) {
      status.set(`Error: ${e}`);
    }
  }

  async function openThree() {
    const base = await pick();
    if (!base) return;
    const local = await pick();
    if (!local) return;
    const remote = await pick();
    if (!remote) return;
    try {
      const view = await api.openThreeWay(base, local, remote);
      registerTab(view, `${basename(base)} (3-way)`);
      status.set(`3-way merge: BASE=${base}`);
    } catch (e) {
      status.set(`Error: ${e}`);
    }
  }
</script>

<button on:click={openTwo}>Open 2-way…</button>
<button on:click={openThree}>Open 3-way merge…</button>
