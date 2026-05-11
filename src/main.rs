// Diffie — native desktop entry point.
//
// The GUI feature pulls in winit + wgpu + imgui. Without it the binary still
// builds (useful for CI / quick syntax checks) but just prints a notice.

#[cfg(feature = "gui")]
fn main() {
    diffie_lib::app::run();
}

#[cfg(not(feature = "gui"))]
fn main() {
    eprintln!(
        "diffie was built without the `gui` feature; rebuild with default features to launch."
    );
}
