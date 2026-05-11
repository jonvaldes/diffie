<script lang="ts">
  import type { TwoWayView, Hunk } from "../lib/types";
  import { api } from "../lib/tauri";
  import { session, pendingAnchor, status } from "../lib/stores";
  import { onDestroy, onMount } from "svelte";
  import HunkControls from "./HunkControls.svelte";
  import Connector from "./Connector.svelte";
  import { hunkOffsets, mappedContentY } from "../lib/scrollSync";
  import { charDiff, leftSegments, rightSegments, type Segment } from "../lib/charDiff";

  export let view: TwoWayView;

  type Row = { lineNo: number | null; segments: Segment[]; cls: string };

  function plain(text: string): Segment[] {
    return [{ text, hl: false }];
  }

  function rowsForHunk(h: Hunk): { left: Row[]; right: Row[] } {
    const left: Row[] = [];
    const right: Row[] = [];

    // Equal hunks: just mirror text on both sides.
    if (!h.ops.some(o => o.kind !== "equal")) {
      for (const op of h.ops) {
        if (op.kind === "equal") {
          left.push({ lineNo: op.a, segments: plain(op.text), cls: "equal" });
          right.push({ lineNo: op.b, segments: plain(op.text), cls: "equal" });
        }
      }
      return { left, right };
    }

    // Change hunks contain only deletes and inserts. Pair them positionally
    // so a delete and the matching insert get character-level highlights.
    const dels = h.ops.filter(o => o.kind === "delete") as Extract<typeof h.ops[number], { kind: "delete" }>[];
    const inss = h.ops.filter(o => o.kind === "insert") as Extract<typeof h.ops[number], { kind: "insert" }>[];
    const pairs = Math.min(dels.length, inss.length);
    for (let i = 0; i < pairs; i++) {
      const runs = charDiff(dels[i].text, inss[i].text);
      left.push({ lineNo: dels[i].a, segments: leftSegments(runs), cls: "delete" });
      right.push({ lineNo: inss[i].b, segments: rightSegments(runs), cls: "insert" });
    }
    for (let i = pairs; i < dels.length; i++) {
      left.push({ lineNo: dels[i].a, segments: [{ text: dels[i].text, hl: true }], cls: "delete" });
    }
    for (let i = pairs; i < inss.length; i++) {
      right.push({ lineNo: inss[i].b, segments: [{ text: inss[i].text, hl: true }], cls: "insert" });
    }
    return { left, right };
  }

  function isChangeHunk(h: Hunk): boolean {
    return h.ops.some(o => o.kind !== "equal");
  }

  function hunkCls(h: Hunk): string {
    return isChangeHunk(h) ? "change" : "equal";
  }

  $: hunkData = view.hunks.map(h => ({ h, rows: rowsForHunk(h) }));

  let host: HTMLDivElement;
  let leftCol: HTMLDivElement;
  let rightCol: HTMLDivElement;

  const leftHunkEls = new Map<number, HTMLDivElement>();
  const rightHunkEls = new Map<number, HTMLDivElement>();
  const leftRowEls = new Map<number, HTMLDivElement>();
  const rightRowEls = new Map<number, HTMLDivElement>();

  // Trigger recomputation in Connector when bindings change.
  let bindTick = 0;
  const bump = () => (bindTick++);

  function bindLeftHunk(node: HTMLDivElement, id: number) {
    leftHunkEls.set(id, node); bump();
    return { destroy() { leftHunkEls.delete(id); bump(); } };
  }
  function bindRightHunk(node: HTMLDivElement, id: number) {
    rightHunkEls.set(id, node); bump();
    return { destroy() { rightHunkEls.delete(id); bump(); } };
  }
  function bindLeftRow(node: HTMLDivElement, line: number | null) {
    if (line == null) return {};
    leftRowEls.set(line, node); bump();
    return { destroy() { leftRowEls.delete(line); bump(); } };
  }
  function bindRightRow(node: HTMLDivElement, line: number | null) {
    if (line == null) return {};
    rightRowEls.set(line, node); bump();
    return { destroy() { rightRowEls.delete(line); bump(); } };
  }

  $: pairs = bindTick >= 0 ? hunkData
    .map(({ h }) => {
      const l = leftHunkEls.get(h.id);
      const r = rightHunkEls.get(h.id);
      if (!l || !r) return null;
      return { id: h.id, leftEl: l, rightEl: r, cls: hunkCls(h) };
    })
    .filter((x): x is { id: number; leftEl: HTMLDivElement; rightEl: HTMLDivElement; cls: string } => x !== null) : [];

  $: anchorPairs = bindTick >= 0 ? view.anchors
    .map(a => {
      const l = leftRowEls.get(a.a);
      const r = rightRowEls.get(a.b);
      if (!l || !r) return null;
      return { leftEl: l, rightEl: r };
    })
    .filter((x): x is { leftEl: HTMLDivElement; rightEl: HTMLDivElement } => x !== null) : [];

  function isAnchored(side: "a" | "b", line: number | null): boolean {
    if (line == null) return false;
    return view.anchors.some(a => (side === "a" ? a.a : a.b) === line);
  }

  // --- Scroll synchronization ---------------------------------------------
  // Identify programmatic writes by remembering the scrollTop value we last
  // wrote to each pane. A scroll event whose current scrollTop still equals
  // that value is the echo of our write — even if the browser fires multiple
  // such events for a single write — so we ignore it. The first time the
  // user actually moves the pane, scrollTop diverges from lastWritten and we
  // resume treating events as user-initiated.
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

  function syncFrom(source: "left" | "right") {
    const srcCol = source === "left" ? leftCol : rightCol;
    const dstCol = source === "left" ? rightCol : leftCol;
    if (!srcCol || !dstCol) return;
    if (isEcho(srcCol)) return;
    const srcMap = source === "left" ? leftHunkEls : rightHunkEls;
    const dstMap = source === "left" ? rightHunkEls : leftHunkEls;
    const srcOffsets = hunkOffsets(srcMap as Map<number, HTMLElement>);
    // Align by viewport center: find what's at the source's center, then
    // place the corresponding destination point at the destination's center.
    const srcCenter = srcCol.scrollTop + srcCol.clientHeight / 2;
    const dstCenter = mappedContentY(srcOffsets, dstMap as Map<number, HTMLElement>, srcCenter);
    if (dstCenter == null) return;
    setScrollSilently(dstCol, dstCenter - dstCol.clientHeight / 2);
  }

  onMount(() => {
    leftCol?.addEventListener("scroll", () => syncFrom("left"), { passive: true });
    rightCol?.addEventListener("scroll", () => syncFrom("right"), { passive: true });
  });

  async function lineClick(side: "a" | "b", line: number | null) {
    if (line == null) return;
    pendingAnchor.update(prev => {
      const without = prev.filter(p => p.side !== side);
      return [...without, { side, line }];
    });
    let p: { side: string; line: number }[] = [];
    pendingAnchor.subscribe(v => (p = v))();
    const a = p.find(x => x.side === "a");
    const b = p.find(x => x.side === "b");
    if (a && b) {
      try {
        const v = await api.addTwoWayAnchor(view.session_id, a.line, b.line);
        session.set(v);
        pendingAnchor.set([]);
        status.set(`Anchor added: A:${a.line} ↔ B:${b.line}`);
      } catch (e) {
        status.set(`Anchor error: ${e}`);
        pendingAnchor.set([]);
      }
    }
  }
</script>

<div class="diff-host" bind:this={host}>
  <div class="diff-col left" bind:this={leftCol}>
    {#each hunkData as { h, rows } (h.id)}
      <div class="hunk-block" use:bindLeftHunk={h.id}>
        {#if isChangeHunk(h)}
          <HunkControls {view} hunkId={h.id} />
        {/if}
        {#each rows.left as r}
          <div class="row {r.cls} {isAnchored('a', r.lineNo) ? 'anchor' : ''}"
               use:bindLeftRow={r.lineNo}
               on:click={() => lineClick("a", r.lineNo)}
               on:keydown={(e) => (e.key === "Enter" || e.key === " ") && lineClick("a", r.lineNo)}
               role="button" tabindex="-1">
            <div class="lineno">{r.lineNo ?? ""}</div>
            <div class="text">{#each r.segments as s}<span class={s.hl ? "char-del" : ""}>{s.text}</span>{/each}{#if r.segments.length === 0 || r.segments.every(s => !s.text)}<span> </span>{/if}</div>
          </div>
        {/each}
      </div>
    {/each}
  </div>

  <Connector {host} {leftCol} {rightCol} {pairs} anchors={anchorPairs} width={56}
             scrollSources={[leftCol, rightCol].filter(Boolean)} />

  <div class="diff-col right" bind:this={rightCol}>
    {#each hunkData as { h, rows } (h.id)}
      <div class="hunk-block" use:bindRightHunk={h.id}>
        {#if isChangeHunk(h)}
          <div class="hunk-controls" style="visibility: hidden">.</div>
        {/if}
        {#each rows.right as r}
          <div class="row {r.cls} {isAnchored('b', r.lineNo) ? 'anchor' : ''}"
               use:bindRightRow={r.lineNo}
               on:click={() => lineClick("b", r.lineNo)}
               on:keydown={(e) => (e.key === "Enter" || e.key === " ") && lineClick("b", r.lineNo)}
               role="button" tabindex="-1">
            <div class="lineno">{r.lineNo ?? ""}</div>
            <div class="text">{#each r.segments as s}<span class={s.hl ? "char-ins" : ""}>{s.text}</span>{/each}{#if r.segments.length === 0 || r.segments.every(s => !s.text)}<span> </span>{/if}</div>
          </div>
        {/each}
      </div>
    {/each}
  </div>
</div>

<style>
  .diff-host {
    display: grid;
    grid-template-columns: 1fr auto 1fr;
    height: calc(100vh - 96px - 240px - 36px);
    overflow: hidden;
    background: var(--bg-2);
    position: relative;
  }
  .diff-col {
    font-family: var(--mono);
    font-size: 12px;
    background: var(--bg-2);
    overflow-y: auto;
    overflow-x: hidden;
    height: 100%;
  }
  .hunk-block { position: relative; }
</style>
