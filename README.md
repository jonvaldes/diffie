# Diffie

**WARNING: THIS PROGRAM IS 100% SLOP, AND I DO NOT TRUST IT. I ADVISE YOU STAY CLEAR OF IT UNTIL IT'S MUCH, MUCH MORE HARDENED.**

## Q: YOU'RE USING THE ABOMINABLE SLOP MACHINE TO BUILD THIS? DO YOU HAVE NO SHAME?

A: Yes, I am using the slop machine. I don't like it. There's so many morally
wrong things with the slop machine that you could fill entire books with them.

And yet I'm using it indeed.

The truth is, I'm using the slop machine because I have a compulsive need to
build things, and between a demanding job and an extremely demanding family
life I can't find time or energy to do it the normal way. And I was going
insane.

So this is really a mental health project for me. It is helping me remain sane.
If it happens to help you too in any way, great!

But if this slop contamination makes you run away and never look back, fair
enough. Whatever lets you keep a bit of your sanity. 

Sending you a big hug from here. I hope you have a wonderful day/week/month/year/life!



## Sloppy summary of the project

A native desktop code diffing & 3-way merge app, written in Rust.

Diffie is built on `imgui-rs` + `wgpu` + `winit` and edits both sides of a diff (or all three sides of a merge) in place. The diff/merge core is a plain library — engine-agnostic, GUI-free, and unit-tested independently of the rendering layer.

## Features

- **2-way diff** with in-place editing of both files, hover overlay for Apply A/B, anchor gutter, bezier ribbons, and same-frame scroll sync.
- **3-way merge** with editable base/local/remote panes and an editable merged-result pane.
- **Pluggable diff engines:** Myers, Patience, Histogram (Histogram supports move detection).
- **Anchors** to force line alignments the engine wouldn't pick on its own.
- **Sub-line refinement** (word / char / grapheme) for paired delete+insert lines.
- **Whitespace handling:** ignore all / leading / trailing-EOL.
- **Syntax highlighting** via tree-sitter for Rust, C++, C#, and HLSL, with rainbow bracket coloring.
- **Per-buffer undo/redo.**
- **Recent comparisons** persisted to the platform's config directory.

## Build & run

```bash
cargo run                                  # launch the GUI app
cargo build                                # build with default (GUI) features
cargo build --no-default-features          # core library only
cargo test  --no-default-features --lib    # fast unit tests, no GUI deps
```

The `gui` Cargo feature is on by default. Disabling it builds the core diff/merge library on its own (useful for CI or embedding).

## CLI

```
diffie                                  Launch with no session
diffie <fileA> <fileB>                  Open a 2-way diff
diffie <base> <fileA> <fileB> <result>  Open a 3-way merge
```

In the 3-way form, `<result>` is bound to the tab as the save target. If the file already exists, its contents are loaded as the current merged result; either way, **Save Result** (Ctrl+S) writes back to that path without prompting. **Save Result As…** still opens a dialog and rebinds the path.

Invalid argument counts print a usage message to stderr and exit with status 2.

## Architecture

```
src/
├── diff/        engine trait, registry, and engines (myers, patience, histogram, anchored)
├── merge.rs     3-way merge over any engine
├── session.rs   SessionStore: per-session state, decisions/resolutions, recompute
├── io.rs        UTF-8 file I/O with encoding fallback
└── app/         GUI: tabs, menu, diff_view, merge_view, syntax, theme, recents
```

See [`CLAUDE.md`](CLAUDE.md) for a more detailed map of the codebase, cross-cutting conventions, and non-obvious invariants.

## Status

Pre-1.0 and under active development. Expect breaking changes in the session/UI APIs.
