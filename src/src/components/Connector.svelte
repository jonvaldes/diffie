<script lang="ts">
  import { afterUpdate, onDestroy, onMount, tick } from "svelte";

  export let host: HTMLElement | undefined;
  export let leftCol: HTMLElement | undefined;
  export let rightCol: HTMLElement | undefined;
  export let pairs: { id: string | number; leftEl: HTMLElement; rightEl: HTMLElement; cls: string }[] = [];
  export let anchors: { leftEl: HTMLElement; rightEl: HTMLElement }[] = [];
  export let width = 48;
  /// Extra scroll sources that should also trigger recompute. Use this when
  /// `host` itself does not scroll but its children do.
  export let scrollSources: HTMLElement[] = [];

  let svgHeight = 0;
  let ribbons: { d: string; cls: string; id: string | number }[] = [];
  let anchorPaths: string[] = [];

  function rectY(el: HTMLElement, baseY: number): { top: number; bot: number } {
    const r = el.getBoundingClientRect();
    return { top: r.top - baseY, bot: r.bottom - baseY };
  }

  function ribbonPath(lTop: number, lBot: number, rTop: number, rBot: number, w: number): string {
    const c = w / 2;
    return [
      `M 0 ${lTop}`,
      `C ${c} ${lTop}, ${c} ${rTop}, ${w} ${rTop}`,
      `L ${w} ${rBot}`,
      `C ${c} ${rBot}, ${c} ${lBot}, 0 ${lBot}`,
      `Z`,
    ].join(" ");
  }

  function linePath(y1: number, y2: number, w: number): string {
    const c = w / 2;
    return `M 0 ${y1} C ${c} ${y1}, ${c} ${y2}, ${w} ${y2}`;
  }

  function recompute() {
    if (!host || !leftCol || !rightCol) return;
    const baseY = host.getBoundingClientRect().top;
    svgHeight = scrollSources.length > 0
      ? host.clientHeight
      : Math.max(
          leftCol.getBoundingClientRect().height,
          rightCol.getBoundingClientRect().height,
        );

    const nextRibbons: typeof ribbons = [];
    for (const p of pairs) {
      if (!p.leftEl || !p.rightEl) continue;
      const l = rectY(p.leftEl, baseY);
      const r = rectY(p.rightEl, baseY);
      nextRibbons.push({ id: p.id, cls: p.cls, d: ribbonPath(l.top, l.bot, r.top, r.bot, width) });
    }
    ribbons = nextRibbons;

    const nextAnchors: string[] = [];
    for (const a of anchors) {
      if (!a.leftEl || !a.rightEl) continue;
      const ly = rectY(a.leftEl, baseY).top;
      const ry = rectY(a.rightEl, baseY).top;
      nextAnchors.push(linePath(ly, ry, width));
    }
    anchorPaths = nextAnchors;
  }

  let ro: ResizeObserver | undefined;

  onMount(async () => {
    await tick();
    recompute();
    ro = new ResizeObserver(() => recompute());
    if (host) ro.observe(host);
    if (leftCol) ro.observe(leftCol);
    if (rightCol) ro.observe(rightCol);
    host?.addEventListener("scroll", recompute, { passive: true });
    for (const s of scrollSources) s.addEventListener("scroll", recompute, { passive: true });
    window.addEventListener("resize", recompute);
  });

  onDestroy(() => {
    ro?.disconnect();
    host?.removeEventListener("scroll", recompute);
    for (const s of scrollSources) s.removeEventListener("scroll", recompute);
    window.removeEventListener("resize", recompute);
  });

  afterUpdate(() => recompute());

  $: if (pairs || anchors) recompute();
</script>

<div class="connector" style="width: {width}px">
  <svg width={width} height={svgHeight} viewBox="0 0 {width} {svgHeight}" preserveAspectRatio="none">
    {#each ribbons as r (r.id)}
      <path d={r.d} class="ribbon {r.cls}" />
    {/each}
    {#each anchorPaths as d, i (i)}
      <path d={d} class="anchor-line" />
    {/each}
  </svg>
</div>

<style>
  .connector {
    position: relative;
    background: var(--bg-3);
    border-left: 1px solid var(--border);
    border-right: 1px solid var(--border);
  }
  svg { display: block; width: 100%; }
  .ribbon {
    stroke-width: 1;
    fill-opacity: 0.35;
    stroke-opacity: 0.55;
  }
  .ribbon.equal {
    fill: var(--border);
    stroke: var(--border);
    fill-opacity: 0.15;
  }
  .ribbon.change {
    fill: var(--accent);
    stroke: var(--accent);
    fill-opacity: 0.28;
  }
  .ribbon.stable {
    fill: var(--border);
    stroke: var(--border);
    fill-opacity: 0.15;
  }
  .ribbon.local_only {
    fill: #2563eb;
    stroke: #2563eb;
    fill-opacity: 0.28;
  }
  .ribbon.remote_only {
    fill: #7c3aed;
    stroke: #7c3aed;
    fill-opacity: 0.28;
  }
  .ribbon.conflict {
    fill: #d97706;
    stroke: #d97706;
    fill-opacity: 0.32;
  }
  .anchor-line {
    fill: none;
    stroke: #000;
    stroke-width: 2.5;
    stroke-linecap: round;
  }
</style>
