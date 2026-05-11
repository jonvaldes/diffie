// Variable-speed scroll synchronization across panes that share hunk ids.
//
// Strategy: for each pane we know the {id, offsetTop, offsetHeight} of every
// hunk. When pane S scrolls to `scrollTop`, we find the hunk that contains
// that y, compute the fraction within it, and align every other pane's
// matching hunk to the same fraction. Hunks of different visual heights
// across panes are what produces the "variable speed" — short hunks on one
// side traverse quickly while the corresponding long hunk on the other side
// traverses slowly.

export interface HunkOffset { id: number | string; top: number; height: number; }

export function hunkOffsets(map: Map<number, HTMLElement>): HunkOffset[] {
  const out: HunkOffset[] = [];
  for (const [id, el] of map) {
    out.push({ id, top: el.offsetTop, height: el.offsetHeight });
  }
  out.sort((a, b) => a.top - b.top);
  return out;
}

/// Locate the hunk containing y (or the last one above y if none contains it).
export function locateHunk(offsets: HunkOffset[], y: number): { idx: number; fraction: number } | null {
  if (offsets.length === 0) return null;
  let i = 0;
  while (i + 1 < offsets.length && offsets[i + 1].top <= y) i++;
  const cur = offsets[i];
  const fraction = cur.height > 0 ? Math.min(1, Math.max(0, (y - cur.top) / cur.height)) : 0;
  return { idx: i, fraction };
}

/// Map a content-y coordinate from the source pane to the equivalent
/// content-y in the destination pane, using matched hunk ids. The result is
/// the y of the corresponding point in the destination's content; callers
/// decide what viewport position (top/center/bottom) it should land at.
export function mappedContentY(
  sourceOffsets: HunkOffset[],
  destMap: Map<number, HTMLElement>,
  sourceY: number,
): number | null {
  const loc = locateHunk(sourceOffsets, sourceY);
  if (!loc) return null;
  const src = sourceOffsets[loc.idx];
  const destEl = destMap.get(src.id as number);
  if (!destEl) return null;
  return destEl.offsetTop + loc.fraction * destEl.offsetHeight;
}
