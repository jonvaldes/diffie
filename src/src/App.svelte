<script lang="ts">
  import { onMount } from "svelte";
  import { save } from "@tauri-apps/plugin-dialog";
  import { api } from "./lib/tauri";
  import { session, status, engines } from "./lib/stores";
  import FilePicker from "./components/FilePicker.svelte";
  import TabBar from "./components/TabBar.svelte";
  import AnchorBar from "./components/AnchorBar.svelte";
  import DiffView from "./components/DiffView.svelte";
  import MergeView from "./components/MergeView.svelte";
  import ResultEditor from "./components/ResultEditor.svelte";

  onMount(async () => {
    try {
      engines.set(await api.availableEngines());
    } catch (e) { status.set(`Init error: ${e}`); }
  });

  async function saveAs() {
    if (!$session) return;
    const path = await save({ title: "Save merged result" });
    if (!path) return;
    try {
      await api.saveResult($session.session_id, path);
      status.set(`Saved: ${path}`);
    } catch (e) { status.set(`Save error: ${e}`); }
  }
</script>

<div class="toolbar">
  <FilePicker />
  <span class="spacer"></span>
  <span style="color: var(--fg-dim)">{$status}</span>
  <button on:click={saveAs} disabled={!$session}>Save Result As…</button>
</div>

<TabBar />

{#if $session}
  {#key $session.session_id}
    <AnchorBar view={$session} />
    {#if $session.mode === "two_way"}
      <DiffView view={$session} />
    {:else}
      <MergeView view={$session} />
    {/if}
    <ResultEditor view={$session} />
  {/key}
{:else}
  <div style="padding: 24px; color: var(--fg-dim)">
    Open two files for a 2-way diff, or three files (base / local / remote) for a 3-way merge.
  </div>
{/if}
