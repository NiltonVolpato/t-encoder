# T-Encoder-Pro launcher — build / flash / test
#
# Replaces upstream's `cargo-esp` wrapper. `export-esp.sh` only sets two things
# (the xtensa-gcc PATH for the linker, and LIBCLANG_PATH), so we derive both
# with globs — that survives toolchain upgrades without editing this file.

set shell := ["bash", "-uc"]

export LIBCLANG_PATH := `ls -d ~/.rustup/toolchains/esp/xtensa-esp32-elf-clang/*/esp-clang/lib 2>/dev/null | head -1`
xtensa_bin := `ls -d ~/.rustup/toolchains/esp/xtensa-esp-elf/*/xtensa-esp-elf/bin 2>/dev/null | head -1`
export PATH := xtensa_bin + ":" + env('PATH')
export RUSTUP_TOOLCHAIN := "esp"

# Device builds need core/alloc from source for the `compiler-builtins-mem`
# intrinsics. Passed here, never in .cargo/config.toml — see that file.
nostd := "-Zbuild-std=alloc,core -Zbuild-std-features=compiler-builtins-mem"
# Host crates are pure and test natively; `--target` overrides [build] target.
host := "--target aarch64-apple-darwin"

_default:
    @just --list

# Build the device firmware.
build *ARGS:
    cargo build -p firmware --release {{nostd}} {{ARGS}}

# Build, flash, and open the serial monitor.
flash *ARGS:
    cargo run -p firmware --release {{nostd}} {{ARGS}}

# Serial monitor only (no flash).
monitor:
    espflash monitor

# Host-side unit tests for our pure crates. `firmware` is device-only, so it is
# excluded; the exclusion list stays correct as pure crates are added.
test *ARGS:
    cargo test {{host}} --workspace --exclude firmware --exclude spike-slint {{ARGS}}

# Host tests for the vendored upstream crates (uses their workspace's
# default-members, which already excludes their device-only crates).
test-vendor *ARGS:
    cargo test {{host}} --manifest-path vendor/rust-enc/Cargo.toml {{ARGS}}

# Clippy on our pure crates (host target, warnings are errors).
lint *ARGS:
    cargo clippy {{host}} --workspace --exclude firmware --exclude spike-slint --all-targets {{ARGS}} -- -D warnings

# Clippy on the device firmware.
lint-device *ARGS:
    cargo clippy -p firmware --release {{nostd}} --all-targets {{ARGS}} -- -D warnings

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

# Everything CI would run.
check: fmt-check lint test build

# Print the toolchain paths this Justfile derived, for debugging.
env-info:
    @echo "LIBCLANG_PATH = $LIBCLANG_PATH"
    @echo "xtensa bin    = {{xtensa_bin}}"
    @rustc --version
