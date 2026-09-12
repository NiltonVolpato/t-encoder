//! Linker setup for the ESP32-S3 target. `linkall.x` (provided by esp-hal)
//! must be the last linker script, so it is appended here rather than via
//! `.cargo/config.toml` rustflags.
#![feature(gethostname)]

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rustc-link-arg=-Tlinkall.x");

    let now = match time::OffsetDateTime::now_local() {
        Ok(now) => now,
        Err(_) => time::OffsetDateTime::now_utc(),
    };
    let descr = time::macros::format_description!("[year]-[month]-[day]");
    let date = now.format(&descr).unwrap_or_default();
    println!("cargo:rustc-env=BUILD_DATE={date}");
    let user = env!("USER");
    println!("cargo:rustc-env=BUILD_USER={user}");
    match std::net::hostname() {
        Ok(s) => println!("cargo:rustc-env=BUILD_HOST={}", s.to_string_lossy()),
        Err(_) => println!("cargo:rustc-env=BUILD_HOST=unknown"),
    }
}
