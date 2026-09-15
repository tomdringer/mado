fn main() {
    slint_build::compile("ui/main.slint").unwrap();

    // Link WebKit so WKWebView/WKWebViewConfiguration classes are available at runtime.
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-lib=framework=WebKit");
}
