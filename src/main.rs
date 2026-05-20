// Diffie — native desktop entry point.
//
// The GUI feature pulls in winit + wgpu + imgui. Without it the binary still
// builds (useful for CI / quick syntax checks) but just prints a notice.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(feature = "gui")]
fn main() {
    use std::path::PathBuf;

    let argv: Vec<String> = std::env::args().collect();
    let prog = argv.first().map(String::as_str).unwrap_or("diffie");
    let args: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();

    if args.iter().any(|a| *a == "-h" || *a == "--help") {
        print_usage(prog, &mut std::io::stdout());
        std::process::exit(0);
    }

    let initial = match args.len() {
        0 => None,
        1 => match diffie_lib::swarm::url::parse(args[0]) {
            Ok(u) => Some(diffie_lib::app::InitialOpen::Swarm(u)),
            Err(_) => {
                print_usage(prog, &mut std::io::stderr());
                std::process::exit(2);
            }
        },
        2 => Some(diffie_lib::app::InitialOpen::TwoWay {
            a: PathBuf::from(args[0]),
            b: PathBuf::from(args[1]),
        }),
        4 => Some(diffie_lib::app::InitialOpen::ThreeWay {
            base: PathBuf::from(args[0]),
            local: PathBuf::from(args[1]),
            remote: PathBuf::from(args[2]),
            result: PathBuf::from(args[3]),
        }),
        _ => {
            print_usage(prog, &mut std::io::stderr());
            std::process::exit(2);
        }
    };

    diffie_lib::app::run_with(initial);
}

#[cfg(feature = "gui")]
fn print_usage<W: std::io::Write>(prog: &str, out: &mut W) {
    let _ = writeln!(out, "Usage:");
    let _ = writeln!(out, "  {prog}                                  Launch with no session");
    let _ = writeln!(out, "  {prog} <fileA> <fileB>                  Open a 2-way diff");
    let _ = writeln!(out, "  {prog} <base> <fileA> <fileB> <result>  Open a 3-way merge");
    let _ = writeln!(out, "  {prog} <swarm-url>                       Open a Swarm review or changelist");
}

#[cfg(not(feature = "gui"))]
fn main() {
    eprintln!(
        "diffie was built without the `gui` feature; rebuild with default features to launch."
    );
}
