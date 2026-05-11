<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { EditorState } from "@codemirror/state";
  import { EditorView, keymap, lineNumbers, highlightActiveLine } from "@codemirror/view";
  import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
  import { api } from "../lib/tauri";
  import { session, status } from "../lib/stores";
  import type { SessionView } from "../lib/types";

  export let view: SessionView;

  let host: HTMLDivElement;
  let editor: EditorView | undefined;
  let lastText = "";
  let debounce: ReturnType<typeof setTimeout> | undefined;

  async function loadInitial() {
    const text = await api.computeResult(view.session_id);
    lastText = text;
    if (editor) {
      editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: text } });
    }
  }

  function pushDebounced(text: string) {
    if (debounce) clearTimeout(debounce);
    debounce = setTimeout(async () => {
      try { await api.updateResult(view.session_id, text); }
      catch (e) { status.set(`Save error: ${e}`); }
    }, 300);
  }

  onMount(async () => {
    const updateListener = EditorView.updateListener.of(u => {
      if (u.docChanged) {
        const text = u.state.doc.toString();
        if (text !== lastText) {
          lastText = text;
          pushDebounced(text);
        }
      }
    });
    editor = new EditorView({
      state: EditorState.create({
        doc: "",
        extensions: [
          lineNumbers(),
          history(),
          highlightActiveLine(),
          keymap.of([...defaultKeymap, ...historyKeymap]),
          updateListener,
        ],
      }),
      parent: host,
    });
    await loadInitial();
  });

  onDestroy(() => editor?.destroy());

  // Reload when session changes (e.g. new decisions applied).
  $: if (view) {
    void (async () => {
      try {
        const text = await api.computeResult(view.session_id);
        if (editor && text !== lastText) {
          lastText = text;
          editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: text } });
        }
      } catch {}
    })();
  }
</script>

<div class="result-pane">
  <h3>Result (editable)</h3>
  <div bind:this={host}></div>
</div>
