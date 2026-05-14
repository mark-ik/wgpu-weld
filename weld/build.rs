// weld uses libloading to load CEF at runtime — there is no link-time
// dependency on libcef. This build script only emits search-path hints
// for tooling convenience; the crate compiles without CEF_PATH set.

fn main() {
    if let Ok(cef_path) = std::env::var("CEF_PATH") {
        println!("cargo:rustc-env=CEF_PATH={cef_path}");
        println!("cargo:rerun-if-env-changed=CEF_PATH");
    }
}
