pub mod diff;
pub mod merge;
pub mod session;
pub mod io;

#[cfg(feature = "gui")]
pub mod app;

// Pure input types and functions (no imgui/gui dependency).
#[cfg(feature = "gui")]
pub use app::input;
