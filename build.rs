// Embed icon + version info into fetchwing.exe on Windows.
fn main() {
    println!("cargo:rerun-if-changed=assets/fetchwing.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/fetchwing.ico")
            .set("ProductName", "Fetchwing")
            .set("FileDescription", "Fetchwing download manager")
            .set("LegalCopyright", "Research project");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed Windows resources: {e}");
        }
    }
}
