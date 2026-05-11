// Diffie — desktop entry point.
//
// The `desktop` feature gates the Tauri runtime so the core library can be
// tested with `cargo test --no-default-features` without needing webkit2gtk
// or other native deps.

#[cfg(feature = "desktop")]
fn main() {
    diffie_lib::commands::run();
}

#[cfg(not(feature = "desktop"))]
fn main() {
    eprintln!(
        "diffie binary requires the `desktop` feature. \
         Run `cargo run --features desktop` or use `cargo tauri dev`."
    );
}
