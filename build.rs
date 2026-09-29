fn main() {
    // On macOS the MenuBar becomes the native menu bar. Elsewhere Slint draws it
    // in-window with the widget style, so force the dark variant to match Mado's
    // dark UI (light text on the bar, dark dropdown menus).
    let mut config = slint_build::CompilerConfiguration::new();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        config = config.with_style("fluent-dark".into());
    }
    slint_build::compile_with_config("ui/main.slint", config).unwrap();

    // Link WebKit so WKWebView/WKWebViewConfiguration classes are available at runtime.
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-lib=framework=WebKit");
}
