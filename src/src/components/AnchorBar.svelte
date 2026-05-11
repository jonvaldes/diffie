<script lang="ts">
  import { api } from "../lib/tauri";
  import { session, pendingAnchor, status } from "../lib/stores";
  import type { SessionView } from "../lib/types";

  export let view: SessionView;

  async function removeAnchor(i: number) {
    if (!view) return;
    try {
      const v = await api.removeAnchor(view.session_id, i);
      session.set(v);
    } catch (e) { status.set(`Error: ${e}`); }
  }

  function clearPending() { pendingAnchor.set([]); }
</script>

<div class="anchorbar">
  <strong>Anchors:</strong>
  {#if view.mode === "two_way"}
    {#each view.anchors as anc, i}
      <span class="anchor-chip">
        A:{anc.a} ↔ B:{anc.b}
        <button on:click={() => removeAnchor(i)} title="Remove">✕</button>
      </span>
    {/each}
  {:else}
    {#each view.anchors as anc, i}
      <span class="anchor-chip">
        BASE:{anc.base} ↔ L:{anc.local} ↔ R:{anc.remote}
        <button on:click={() => removeAnchor(i)} title="Remove">✕</button>
      </span>
    {/each}
  {/if}

  {#if $pendingAnchor.length > 0}
    <span class="anchor-chip">
      Picking: {$pendingAnchor.map(p => `${p.side}:${p.line}`).join(" + ")}
      <button on:click={clearPending}>Cancel</button>
    </span>
  {:else}
    <span style="color: var(--fg-dim)">
      Click a line in each pane to set an anchor.
    </span>
  {/if}
</div>
