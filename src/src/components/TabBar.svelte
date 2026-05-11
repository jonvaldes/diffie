<script lang="ts">
  import { api } from "../lib/tauri";
  import { tabs, activeTabId, session, status, pendingAnchor } from "../lib/stores";

  async function activate(sessionId: number) {
    if ($activeTabId === sessionId) return;
    try {
      const v = await api.getSession(sessionId);
      activeTabId.set(sessionId);
      session.set(v);
      pendingAnchor.set([]);
    } catch (e) {
      status.set(`Tab error: ${e}`);
    }
  }

  async function close(sessionId: number, ev: MouseEvent) {
    ev.stopPropagation();
    let nextActive: number | null = null;
    tabs.update(list => {
      const idx = list.findIndex(t => t.sessionId === sessionId);
      const next = list.filter(t => t.sessionId !== sessionId);
      if ($activeTabId === sessionId && next.length > 0) {
        const fallback = next[Math.min(idx, next.length - 1)];
        nextActive = fallback.sessionId;
      } else if (next.length === 0) {
        nextActive = null;
      } else {
        nextActive = $activeTabId;
      }
      return next;
    });
    if (nextActive === null) {
      activeTabId.set(null);
      session.set(null);
      pendingAnchor.set([]);
    } else if (nextActive !== $activeTabId) {
      await activate(nextActive);
    }
  }
</script>

{#if $tabs.length > 0}
  <div class="tabbar">
    {#each $tabs as t (t.sessionId)}
      <div class="tab {t.sessionId === $activeTabId ? 'active' : ''}"
           on:click={() => activate(t.sessionId)}
           on:keydown={(e) => (e.key === 'Enter' || e.key === ' ') && activate(t.sessionId)}
           role="button" tabindex="0"
           title={t.label}>
        <span class="badge {t.mode}">{t.mode === 'two_way' ? '2' : '3'}</span>
        <span class="label">{t.label}</span>
        <button class="close" on:click={(e) => close(t.sessionId, e)} title="Close tab">✕</button>
      </div>
    {/each}
  </div>
{/if}

<style>
  .tabbar {
    display: flex;
    align-items: stretch;
    gap: 2px;
    padding: 0 8px;
    background: var(--bg-3);
    border-bottom: 1px solid var(--border);
    overflow-x: auto;
    min-height: 32px;
  }
  .tab {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 8px 4px 6px;
    background: var(--bg-2);
    border: 1px solid var(--border);
    border-bottom: none;
    border-top-left-radius: 6px;
    border-top-right-radius: 6px;
    cursor: pointer;
    font-size: 12px;
    color: var(--fg-dim);
    max-width: 280px;
    margin-top: 4px;
    user-select: none;
  }
  .tab:hover { background: var(--bg-1, var(--bg-2)); color: var(--fg, inherit); }
  .tab.active {
    background: var(--bg-2);
    color: var(--fg, inherit);
    border-color: var(--accent);
    border-bottom: 2px solid var(--bg-2);
    margin-bottom: -1px;
  }
  .label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 220px;
  }
  .badge {
    font-size: 10px;
    font-weight: 700;
    padding: 1px 5px;
    border-radius: 8px;
    background: var(--border);
    color: var(--bg-2);
  }
  .badge.two_way { background: var(--accent, #2563eb); color: #fff; }
  .badge.three_way { background: #d97706; color: #fff; }
  .close {
    background: transparent;
    border: none;
    color: inherit;
    cursor: pointer;
    padding: 0 2px;
    font-size: 12px;
    line-height: 1;
    opacity: 0.6;
  }
  .close:hover { opacity: 1; color: #ef4444; }
</style>
