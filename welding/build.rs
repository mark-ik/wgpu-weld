// Under the `cef-runtime` feature, `cef-dll-sys` downloads and links libcef
// from the configured CEF binary distribution. This build script just emits
// CEF_PATH as a rustc env for tooling convenience and triggers rebuilds when
// it changes.

fn main() {
    if let Ok(cef_path) = std::env::var("CEF_PATH") {
        println!("cargo:rustc-env=CEF_PATH={cef_path}");
        println!("cargo:rerun-if-env-changed=CEF_PATH");
    }
}
