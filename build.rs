fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/diffie.ico");
        if let Err(e) = res.compile() {
            eprintln!("failed to embed windows resources: {e}");
            std::process::exit(1);
        }
    }
}
