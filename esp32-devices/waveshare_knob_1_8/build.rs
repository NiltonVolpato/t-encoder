fn main() {
    println!("cargo::rustc-check-cfg=cfg(rust_analyzer)");
    println!("cargo::rustc-link-arg=-Tdefmt.x");
    // make sure linkall.x is the last linker script (otherwise might cause problems with flip-link)
    println!("cargo::rustc-link-arg=-Tlinkall.x");
    println!("cargo::rerun-if-changed=build.rs");
}
