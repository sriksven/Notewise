//! Link configuration for the macOS capture backends.
//!
//! ScreenCaptureKit reaches this crate through bindings that carry a Swift shim, and Swift's
//! own runtime (`libswift_Concurrency.dylib` and friends) lives in the dyld shared cache under
//! `/usr/lib/swift` rather than anywhere the default search path looks. Without this, anything
//! linking the `os-capture` feature builds cleanly and then aborts at startup with
//! "Library not loaded: @rpath/libswift_Concurrency.dylib".

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Read from the environment rather than `cfg!`, which in a build script describes the host
    // rather than the target.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let os_capture = std::env::var("CARGO_FEATURE_OS_CAPTURE").is_ok();

    if target_os == "macos" && os_capture {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
