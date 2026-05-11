// Character-level diff between two strings via LCS. Used to highlight the
// specific parts of a deleted/inserted line pair that actually changed.
//
// Capped: if either side is unusually long we bail and return whole-line
// runs so the caller falls back to row-level highlighting only.

export type CharRun = { kind: "equal" | "del" | "ins"; text: string };

const MAX_LEN = 600;

export function charDiff(a: string, b: string): CharRun[] {
  if (a === b) return a ? [{ kind: "equal", text: a }] : [];
  if (a.length === 0) return [{ kind: "ins", text: b }];
  if (b.length === 0) return [{ kind: "del", text: a }];
  if (a.length > MAX_LEN || b.length > MAX_LEN) {
    return [{ kind: "del", text: a }, { kind: "ins", text: b }];
  }

  const m = a.length;
  const n = b.length;
  const w = n + 1;
  // dp[i][j] = LCS length of a[i..] and b[j..]
  const dp = new Uint32Array((m + 1) * w);
  for (let i = m - 1; i >= 0; i--) {
    for (let j = n - 1; j >= 0; j--) {
      dp[i * w + j] = a.charCodeAt(i) === b.charCodeAt(j)
        ? dp[(i + 1) * w + (j + 1)] + 1
        : Math.max(dp[(i + 1) * w + j], dp[i * w + (j + 1)]);
    }
  }

  const runs: CharRun[] = [];
  let curKind: CharRun["kind"] | null = null;
  let curText = "";
  const push = (k: CharRun["kind"], c: string) => {
    if (curKind === k) curText += c;
    else {
      if (curKind !== null) runs.push({ kind: curKind, text: curText });
      curKind = k;
      curText = c;
    }
  };

  let i = 0, j = 0;
  while (i < m && j < n) {
    if (a.charCodeAt(i) === b.charCodeAt(j)) {
      push("equal", a[i]); i++; j++;
    } else if (dp[(i + 1) * w + j] >= dp[i * w + (j + 1)]) {
      push("del", a[i]); i++;
    } else {
      push("ins", b[j]); j++;
    }
  }
  while (i < m) { push("del", a[i++]); }
  while (j < n) { push("ins", b[j++]); }
  if (curKind !== null) runs.push({ kind: curKind, text: curText });
  return runs;
}

export type Segment = { text: string; hl: boolean };

/// Segments for the left/delete side of a paired change.
export function leftSegments(runs: CharRun[]): Segment[] {
  return runs
    .filter(r => r.kind !== "ins")
    .map(r => ({ text: r.text, hl: r.kind === "del" }));
}

/// Segments for the right/insert side of a paired change.
export function rightSegments(runs: CharRun[]): Segment[] {
  return runs
    .filter(r => r.kind !== "del")
    .map(r => ({ text: r.text, hl: r.kind === "ins" }));
}
