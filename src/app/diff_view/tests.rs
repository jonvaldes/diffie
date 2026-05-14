//! Tests for the 2-way diff view.
//!
//! The pre-multiline-rewrite tests exercised the per-row pipeline
//! (`build_pane`, `draw_row`, `update_selection`, the splice handler,
//! drag-selection state machine, etc.) which were deleted in the
//! Task-5 rewrite. Those tests are REMOVED in multiline rewrite —
//! see task 11 for the new test suite that targets the
//! two-`input_text_multiline` shape.
