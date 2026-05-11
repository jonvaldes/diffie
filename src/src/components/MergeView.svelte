<script lang="ts">
  import { onMount } from "svelte";
  import type { ThreeWayView, MergeHunk, Resolution } from "../lib/types";
  import { api } from "../lib/tauri";
  import { session, status } from "../lib/stores";
  import Connector from "./Connector.svelte";
  import { hunkOffsets, mappedContentY } from "../lib/scrollSync";

  export let view: ThreeWayView;

  type Row = { line: number; text: string; cls: string };
  type PaneHunk = { id: number; kind: MergeHunk["kind"]; rows: Row[] };

  function buildPane(hunks: MergeHunk[], pick: (h: MergeHunk) => { text: string[]; cls: string }): PaneHunk[] {
    let n = 1;
    return hunks.map(h => {
      const { text, cls } = pick(h);
      const rows: Row[] = text.map(t => ({ line: n++, text: t, cls }));
      return { id: h.id, kind: h.kind, rows };
    });
  }

  $: basePaneHunks = buildPane(view.hunks, h => {
    if (h.kind === "stable") return { text: h.text, cls: "equal" };
    return { text: h.base, cls: h.kind };
  });

  $: localPaneHunks = buildPane(view.hunks, h => {
    if (h.kind === "stable") return { text: h.text, cls: "equal" };
    if (h.kind === "local_only" || h.kind === "conflict") return { text: h.local, cls: h.kind };
    return { text: h.base, cls: "equal" };
  });

  $: remotePaneHunks = buildPane(view.hunks, h => {
    if (h.kind === "stable") return { text: h.text, cls: "equal" };
    if (h.kind === "remote_only" || h.kind === "conflict") return { text: h.remote, cls: h.kind };
    return { text: h.base, cls: "equal" };
  });

  async function setRes(hunkId: number, kind: Resolution["kind"]) {
    try {
      const res: Resolution = { kind } as Resolution;
      await api.setThreeWayResolution(view.session_id, hunkId, res);
      const v = await api.getSession(view.session_id);
      session.set(v);
    } catch (e) { status.set(`Error: ${e}`); }
  }

  let host: HTMLDivElement;
  let basePane: HTMLDivElement;
  let localPane: HTMLDivElement;
  let remotePane: HTMLDivElement;

  type Pane = "base" | "local" | "remote";
  const hunkEls: Record<Pane, Map<number, HTMLDivElement>> = {
    base: new Map(), local: new Map(), remote: new Map(),
  };
  const rowEls: Record<Pane, Map<number, HTMLDivElement>> = {
    base: new Map(), local: new Map(), remote: new Map(),
  };

  let bindTick = 0;
  const bump = () => (bindTick++);

  function bindHunk(pane: Pane) {
    return (node: HTMLDivElement, id: number) => {
      hunkEls[pane].set(id, node); bump();
      return { destroy() { hunkEls[pane].delete(id); bump(); } };
    };
  }
  function bindRow(pane: Pane) {
    return (node: HTMLDivElement, line: number) => {
      rowEls[pane].set(line, node); bump();
      return { destroy() { rowEls[pane].delete(line); bump(); } };
    };
  }
  const bindBaseHunk = bindHunk("base");
  const bindLocalHunk = bindHunk("local");
  const bindRemoteHunk = bindHunk("remote");
  const bindBaseRow = bindRow("base");
  const bindLocalRow = bindRow("local");
  const bindRemoteRow = bindRow("remote");

  function ribbonCls(kind: MergeHunk["kind"]): string {
    return kind;
  }

  function makePairs(leftMap: Map<number, HTMLDivElement>, rightMap: Map<number, HTMLDivElement>) {
    return view.hunks
      .map(h => {
        const l = leftMap.get(h.id);
        const r = rightMap.get(h.id);
        if (!l || !r) return null;
        return { id: h.id, leftEl: l, rightEl: r, cls: ribbonCls(h.kind) };
      })
      .filter((x): x is { id: number; leftEl: HTMLDivElement; rightEl: HTMLDivElement; cls: string } => x !== null);
  }

  function makeAnchors(leftPane: Pane, rightPane: Pane, pick: (a: typeof view.anchors[number]) => { l: number; r: number }) {
    return view.anchors
      .map(a => {
        const p = pick(a);
        const l = rowEls[leftPane].get(p.l);
        const r = rowEls[rightPane].get(p.r);
        if (!l || !r) return null;
        return { leftEl: l, rightEl: r };
      })
      .filter((x): x is { leftEl: HTMLDivElement; rightEl: HTMLDivElement } => x !== null);
  }

  $: pairsBL = bindTick >= 0 ? makePairs(hunkEls.base, hunkEls.local) : [];
  $: pairsLR = bindTick >= 0 ? makePairs(hunkEls.local, hunkEls.remote) : [];
  $: anchorsBL = bindTick >= 0 ? makeAnchors("base", "local", a => ({ l: a.base, r: a.local })) : [];
  $: anchorsLR = bindTick >= 0 ? makeAnchors("local", "remote", a => ({ l: a.local, r: a.remote })) : [];

  function isAnchored(pane: Pane, line: number): boolean {
    return view.anchors.some(a => a[pane] === line);
  }

  // --- Scroll synchronization ---------------------------------------------
  const lastWritten = new WeakMap<HTMLElement, number>();
  function setScrollSilently(el: HTMLElement, y: number) {
    const cur = Math.round(el.scrollTop);
    const target = Math.round(y);
    if (cur === target) return;
    el.scrollTop = y;
    lastWritten.set(el, Math.round(el.scrollTop));
  }
  function isEcho(el: HTMLElement): boolean {
    const w = lastWritten.get(el);
    return w !== undefined && Math.round(el.scrollTop) === w;
  }
  function paneEl(p: Pane): HTMLDivElement | undefined {
    return p === "base" ? basePane : p === "local" ? localPane : remotePane;
  }
  function syncFrom(source: Pane) {
    const srcCol = paneEl(source);
    if (!srcCol) return;
    if (isEcho(srcCol)) return;
    const others: Pane[] = (["base", "local", "remote"] as Pane[]).filter(p => p !== source);
    const srcOffsets = hunkOffsets(hunkEls[source] as Map<number, HTMLElement>);
    const srcCenter = srcCol.scrollTop + srcCol.clientHeight / 2;
    for (const dst of others) {
      const dstCol = paneEl(dst);
      const dstMap = hunkEls[dst] as Map<number, HTMLElement>;
      if (!dstCol) continue;
      const dstCenter = mappedContentY(srcOffsets, dstMap, srcCenter);
      if (dstCenter != null) setScrollSilently(dstCol, dstCenter - dstCol.clientHeight / 2);
    }
  }

  onMount(() => {
    basePane?.addEventListener("scroll", () => syncFrom("base"), { passive: true });
    localPane?.addEventListener("scroll", () => syncFrom("local"), { passive: true });
    remotePane?.addEventListener("scroll", () => syncFrom("remote"), { passive: true });
  });
</script>

<div class="merge-host" bind:this={host}>
  <div class="pane" bind:this={basePane}>
    <div class="pane-header"><strong>BASE</strong></div>
    {#each basePaneHunks as ph (ph.id)}
      <div class="hunk {ph.kind}" use:bindBaseHunk={ph.id}>
        {#each ph.rows as r (r.line)}
          <div class="row {r.cls} {isAnchored('base', r.line) ? 'anchor' : ''}"
               use:bindBaseRow={r.line}>
            <div class="lineno">{r.line}</div>
            <div class="text">{r.text || " "}</div>
          </div>
        {/each}
      </div>
    {/each}
  </div>

  <Connector host={host} leftCol={basePane} rightCol={localPane} pairs={pairsBL} anchors={anchorsBL} width={56}
             scrollSources={[basePane, localPane].filter(Boolean)} />

  <div class="pane" bind:this={localPane}>
    <div class="pane-header"><strong>LOCAL</strong></div>
    {#each localPaneHunks as ph (ph.id)}
      <div class="hunk {ph.kind}" use:bindLocalHunk={ph.id}>
        {#if ph.kind === "local_only" || ph.kind === "conflict"}
          <div class="hunk-controls">
            <button on:click={() => setRes(ph.id, "local")}>Use Local</button>
            <button on:click={() => setRes(ph.id, "base")}>Use Base</button>
            {#if ph.kind === "conflict"}
              <button on:click={() => setRes(ph.id, "remote")}>Use Remote</button>
            {/if}
          </div>
        {/if}
        {#each ph.rows as r (r.line)}
          <div class="row {r.cls} {isAnchored('local', r.line) ? 'anchor' : ''}"
               use:bindLocalRow={r.line}>
            <div class="lineno">{r.line}</div>
            <div class="text">{r.text || " "}</div>
          </div>
        {/each}
      </div>
    {/each}
  </div>

  <Connector host={host} leftCol={localPane} rightCol={remotePane} pairs={pairsLR} anchors={anchorsLR} width={56}
             scrollSources={[localPane, remotePane].filter(Boolean)} />

  <div class="pane" bind:this={remotePane}>
    <div class="pane-header"><strong>REMOTE</strong></div>
    {#each remotePaneHunks as ph (ph.id)}
      <div class="hunk {ph.kind}" use:bindRemoteHunk={ph.id}>
        {#each ph.rows as r (r.line)}
          <div class="row {r.cls} {isAnchored('remote', r.line) ? 'anchor' : ''}"
               use:bindRemoteRow={r.line}>
            <div class="lineno">{r.line}</div>
            <div class="text">{r.text || " "}</div>
          </div>
        {/each}
      </div>
    {/each}
  </div>
</div>

<style>
  .merge-host {
    display: grid;
    grid-template-columns: 1fr auto 1fr auto 1fr;
    height: calc(100vh - 96px - 240px - 36px);
    overflow: hidden;
    background: var(--bg-2);
    position: relative;
  }
  .pane {
    font-family: var(--mono);
    font-size: 12px;
    background: var(--bg-2);
    overflow-y: auto;
    overflow-x: hidden;
    height: 100%;
  }
  .pane-header { padding: 4px 8px; background: var(--bg-3); border-bottom: 1px solid var(--border); }
  .hunk { position: relative; }
  .hunk.stable { background: transparent; }
  .hunk.local_only { background: rgba(37, 99, 235, 0.06); }
  .hunk.remote_only { background: rgba(124, 58, 237, 0.06); }
  .hunk.conflict { background: rgba(217, 119, 6, 0.08); }
</style>
