//! Embeds `assets/desktop-tools.ico` into the Windows exe.

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }
    winresource::WindowsResource::new()
        .set_icon("assets/desktop-tools.ico")
        .compile()
        .expect("cannot compile the icon resource");
}
