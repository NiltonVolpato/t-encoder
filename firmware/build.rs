//! Linker setup for the ESP32-S3 target. `linkall.x` (provided by esp-hal)
//! must be the last linker script, so it is appended here rather than via
//! `.cargo/config.toml` rustflags.

fn main() {
    println!("cargo:rustc-link-arg=-Tlinkall.x");
}
