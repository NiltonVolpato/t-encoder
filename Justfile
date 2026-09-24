# T-Encoder Workspace Justfile
#
# Supports two decoupled workspaces:
#  1. Host workspace (root `./`): Pure crates, apps, tests, and simulators.
#  2. Device workspace (`./esp32-devices`): Embedded crates (waveshare_knob_1_8, lilygo_t_encoder_pro, common).
#
# Flashing and monitoring use probe-rs via `cargo run`.

set shell := ["bash", "-uc"]
set unstable := true
set lists := true
set dotenv-load := true
set dotenv-filename := [".env", ".env.local"]
set dotenv-override := true

# Ensure xtensa GCC toolchain is in PATH for the linker
xtensa_bin := `ls -d ~/.rustup/toolchains/esp/xtensa-esp-elf/*/xtensa-esp-elf/bin 2>/dev/null | head -1`
export PATH := xtensa_bin + ":" + env('PATH')

# Default target device (can be overridden with DEVICE=lilygo_t_encoder_pro or recipe argument)
export DEVICE := env('DEVICE', "waveshare_knob_1_8")

_default:
    @just --list

# -----------------------------------------------------------------------------
# Host Workspace Recipes (pure crates, apps, tests)
# -----------------------------------------------------------------------------

[doc("Run host unit tests for apps and pure crates.")]
[group("verification")]
test *ARGS:
    cargo test --workspace {{ARGS}}

[doc("Clippy on host crates (warnings are errors).")]
[group("verification")]
lint *ARGS:
    cargo clippy --workspace --all-targets {{ARGS}} -- -D warnings

[doc("Typecheck host crates.")]
[group("verification")]
check-native *ARGS:
    cargo check --workspace --all-targets {{ARGS}}

[doc("Format all Rust files in both workspaces.")]
[group("verification")]
fmt:
    cargo +nightly fmt --all
    cd esp32-devices && cargo +nightly fmt --all

[doc("Check formatting without modifying files.")]
[group("verification")]
fmt-check:
    cargo +nightly fmt --all -- --check
    cd esp32-devices && cargo +nightly fmt --all -- --check

# -----------------------------------------------------------------------------
# Apps & Simulators (host with Slint MCP)
# -----------------------------------------------------------------------------

[doc("Run a host app simulator with Slint MCP enabled (e.g. just test-app clock, default port: 3450).")]
[group("apps")]
test-app APP="app-clock" PORT="3450" *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    pkg="{{APP}}"
    if [[ ! "$pkg" =~ ^app- ]] && [[ -d "apps/$pkg" ]]; then
        pkg="app-$pkg"
    fi
    echo "Starting simulator for $pkg on Slint MCP port {{PORT}}..."
    SLINT_MCP_PORT="{{PORT}}" cargo run -p "$pkg" {{ARGS}}

alias run-app := test-app

# -----------------------------------------------------------------------------
# Device Workspace Recipes (esp32-devices)
# -----------------------------------------------------------------------------


[doc("Build device firmware in release mode (default: waveshare_knob_1_8).")]
[group("device")]
build TARGET=DEVICE *ARGS:
    cd esp32-devices && cargo build -p {{TARGET}} --release {{ARGS}}

[doc("Build, flash, and monitor device firmware using probe-rs (cargo run).")]
[group("device")]
flash TARGET=DEVICE *ARGS:
    cd esp32-devices && cargo run -p {{TARGET}} --release {{ARGS}}

[doc("Typecheck device firmware crates.")]
[group("device")]
check-device TARGET=DEVICE *ARGS:
    cd esp32-devices && cargo check -p {{TARGET}} {{ARGS}}

[doc("Clippy on device firmware crates.")]
[group("device")]
lint-device TARGET=DEVICE *ARGS:
    cd esp32-devices && cargo clippy -p {{TARGET}} --release {{ARGS}} -- -D warnings

[doc("Show firmware size breakdown by sections and compile units using bloaty.")]
[group("device")]
size TARGET=DEVICE COUNT="20":
    bloaty esp32-devices/target/xtensa-esp32s3-none-elf/release/{{TARGET}} -d sections -n {{COUNT}}
    @echo ""
    bloaty esp32-devices/target/xtensa-esp32s3-none-elf/release/{{TARGET}} -d compileunits -n {{COUNT}}

[doc("Run any cargo or tool command inside the esp32-devices workspace.")]
[group("device")]
exec-device *COMMAND:
    cd esp32-devices && {{COMMAND}}

# -----------------------------------------------------------------------------
# Profiling & Diagnostics
# -----------------------------------------------------------------------------

[doc("Flash with profiler armed, pipe output to profile.log, and symbolize hotspots (e.g. just flash-profile 10s).")]
[group("profile")]
flash-profile OPTS="input,10s" TARGET=DEVICE *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    cd esp32-devices
    echo "Starting profiling run with PROFILE={{OPTS}} on {{TARGET}}..."
    # Allow probe-rs / cargo run to exit or be stopped via Ctrl-C without aborting symbolizing
    PROFILE="{{OPTS}}" cargo run -p {{TARGET}} --release {{ARGS}} 2>&1 | tee profile.log || true
    cd ..
    if [[ -s esp32-devices/profile.log ]]; then
        echo ""
        echo "================ Symbolizing Captured Profile ================"
        python3 tools/symbolize_profile.py < esp32-devices/profile.log
    fi

[doc("Symbolize an existing profiler log file (default: esp32-devices/profile.log).")]
[group("profile")]
symbolize LOG="esp32-devices/profile.log":
    python3 tools/symbolize_profile.py < {{LOG}}

# -----------------------------------------------------------------------------
# Verification Suite & Info
# -----------------------------------------------------------------------------

[doc("Typecheck both native crates and device firmware.")]
[group("verification")]
check: check-native check-device

[doc("Run all verification steps: formatting check, lints, tests, and build.")]
[group("verification")]
verify: fmt-check lint lint-device test (build "waveshare_knob_1_8") (build "lilygo_t_encoder_pro")

[doc("Print toolchain and environment diagnostic info.")]
[group("debug")]
env-info:
    @echo "=== Host Toolchain ==="
    @rustc --version
    @echo ""
    @echo "=== Device Toolchain (esp32-devices) ==="
    @echo "xtensa bin = {{xtensa_bin}}"
    @cd esp32-devices && rustc --version

