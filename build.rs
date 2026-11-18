// Embeds a manifest that opts into Common Controls v6 (modern edit field,
// placeholder text). DPI awareness is set at runtime in main().
fn main() {
    // App icon (resource 1), from assets/logo.
    println!("cargo:rerun-if-changed=assets/veyra.rc");
    println!("cargo:rerun-if-changed=assets/logo/veyra.ico");
    embed_resource::compile("assets/veyra.rc", embed_resource::NONE).manifest_optional().expect("app icon resource");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' \
             version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
    }
}
