<script lang="ts">
  import { api } from "../lib/tauri";
  import { session, status } from "../lib/stores";
  import type { HunkDecision, SessionView } from "../lib/types";

  export let view: SessionView & { mode: "two_way" };
  export let hunkId: number;

  async function setDecision(kind: HunkDecision["kind"]) {
    try {
      const dec: HunkDecision = { kind } as HunkDecision;
      await api.setTwoWayDecision(view.session_id, hunkId, dec);
      // Refresh session to reflect computed result later.
      const v = await api.getSession(view.session_id);
      session.set(v);
    } catch (e) { status.set(`Error: ${e}`); }
  }
</script>

<div class="hunk-controls">
  <button on:click={() => setDecision("accept_a")}>← A</button>
  <button on:click={() => setDecision("accept_b")}>B →</button>
  <button on:click={() => setDecision("both")}>Both</button>
  <button on:click={() => setDecision("neither")}>Neither</button>
</div>
