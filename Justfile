# T-Encoder-Pro launcher — build / flash / test
#
# Replaces upstream's `cargo-esp` wrapper. `export-esp.sh` only sets two things
# (the xtensa-gcc PATH for the linker, and LIBCLANG_PATH), so we derive both
# with globs — that survives toolchain upgrades without editing this file.

set shell := ["bash", "-uc"]


set unstable := true
set lists := true
set dotenv-load := true
set dotenv-filename := [".env", ".env.local"]
set dotenv-override := true

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

# Build, flash, and open the serial monitor. Interactive: runs until you quit.
flash *ARGS:
    cargo run -p firmware --release {{nostd}} {{ARGS}}

# Flash without waiting for the app — for when the logs do not matter, or the
# build may never reach its boot marker. espflash needs a TTY for its input
# reader, so with stdin redirected it flashes and then dies on the monitor,
# which is the point; but that also means a *successful* flash exits non-zero,
# so the outcome comes from espflash's own completion line instead.
flash-only *ARGS:
    #!/usr/bin/env bash
    set -uo pipefail
    out=$(cargo run -p firmware --release {{nostd}} {{ARGS}} < /dev/null 2>&1)
    echo "$out"
    grep -q 'Flashing has completed' <<< "$out"

# Flash, stream the boot log, and exit as soon as the app reports MARKER —
# `just flash` never terminates on its own, which makes it useless from a
# script. Fails fast (non-zero) on timeout or if the app gives up on the
# display. TIMEOUT is in seconds and covers the build too.
flash-log MARKER='boot: ready' TIMEOUT='180':
    #!/usr/bin/env expect -f

    # Tear the whole tree down — just, cargo and espflash. Every exit path goes
    # through here: a leftover espflash holds the serial port and makes the
    # *next* flash fail with an unexplained "Failed to open serial port".
    # Ctrl-C alone reaches only the monitor, but it exits quietly, so escalate
    # to signalling the process group (negative pid — `spawn` gives the child
    # its own session) only if that does not take.
    proc shutdown {pid code} {
        global spawn_id
        catch { send \003 }
        expect -timeout 10 {
            eof {}
            timeout {
                catch { exec kill -TERM -$pid }
                catch { expect -timeout 5 eof }
            }
        }
        catch { wait }
        exit $code
    }

    set timeout {{TIMEOUT}}
    set pid [spawn just flash]
    expect {
        "{{MARKER}}" { shutdown $pid 0 }
        "display unavailable" {
            send_user "\n*** app came up without a display\n"
            shutdown $pid 1
        }
        timeout {
            send_user "\n*** timed out waiting for '{{MARKER}}'\n"
            shutdown $pid 1
        }
        eof {
            send_user "\n*** exited before '{{MARKER}}'\n"
            exit 1
        }
    }

# Serial monitor only (no flash).
monitor:
    espflash monitor

# Host-side unit tests for our pure crates. `firmware` is device-only, so it is
# excluded; the exclusion list stays correct as pure crates are added.
test *ARGS:
    cargo test {{host}} --workspace --exclude firmware --exclude ui --exclude apps {{ARGS}}

# Host tests for the vendored upstream crates (uses their workspace's
# default-members, which already excludes their device-only crates).
test-vendor *ARGS:
    cargo test {{host}} --manifest-path vendor/rust-enc/Cargo.toml {{ARGS}}

# Clippy on our pure crates (host target, warnings are errors).
lint *ARGS:
    cargo clippy {{host}} --workspace --exclude firmware --exclude ui --exclude apps --all-targets {{ARGS}} -- -D warnings

# Clippy on the device firmware.
lint-device *ARGS:
    cargo clippy -p firmware --release {{nostd}} --all-targets {{ARGS}} -- -D warnings

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

# Everything CI would run.
check: fmt-check lint test build

doc PACKAGE:
    #!/bin/sh
    set -e
    RUSTDOCFLAGS="-Z unstable-options --output-format json" cargo doc --release {{nostd}} --no-deps --package {{PACKAGE}}
    CRATE=$(echo {{PACKAGE}} | tr '-' '_')
    rustdoc-md --path target/xtensa-esp32s3-none-elf/doc/${CRATE}.json --output target/xtensa-esp32s3-none-elf/doc/${CRATE}.md
    mkdir -p target/doc
    ln -sf ../xtensa-esp32s3-none-elf/doc/${CRATE}.md target/doc/${CRATE}.md
    echo "Documentation written to target/doc/${CRATE}.md"

# Print the toolchain paths this Justfile derived, for debugging.
env-info:
    @echo "LIBCLANG_PATH = $LIBCLANG_PATH"
    @echo "xtensa bin    = {{xtensa_bin}}"
    @rustc --version
