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

xtensa_bin := `ls -d ~/.rustup/toolchains/esp/xtensa-esp-elf/*/xtensa-esp-elf/bin 2>/dev/null | head -1`
export PATH := xtensa_bin + ":" + env('PATH')

# Set up environment variables for device builds by default.
# Device builds need core/alloc from source for the `compiler-builtins-mem` intrinsics.
#
# LIBCLANG_PATH is one of the two things export-esp.sh sets, so we carry it, but
# nothing in the current graph consumes it: the only lockfile entry wanting
# bindgen is skia-bindings, a Slint backend we do not enable. Keep it for the
# day a dep does need libclang; do not be surprised that unsetting it changes
# nothing today.
export LIBCLANG_PATH := `ls -d ~/.rustup/toolchains/esp/xtensa-esp32-elf-clang/*/esp-clang/lib 2>/dev/null | head -1`
export CARGO_BUILD_TARGET := "xtensa-esp32s3-none-elf"
export RUSTUP_TOOLCHAIN := "esp"
export CARGO_UNSTABLE_BUILD_STD := "alloc,core"
export CARGO_UNSTABLE_BUILD_STD_FEATURES := "compiler-builtins-mem"

# Flashing-related variables:
# Native USB-Serial/JTAG. Note that:
#   --partition-table  the custom table adds the `settings` partition; without
#                      it, settings persistence silently stops working
#   --after hard-reset resets into the app after flashing. Without it the chip
#                      is left sitting in ROM download mode ("waiting for
#                      download") with a black screen, needing a manual reset
#   --port             explicit; auto-detect can hang waiting on a device probe.
#                      it can be overridden with `FLASH_PORT` in `.env.local`.
FLASH_PORT := env('FLASH_PORT', "/dev/cu.usbmodem101")
FIRMWARE_PATH := justfile_directory() + "/target/xtensa-esp32s3-none-elf/release/firmware"
FLASH_ARGS := "--chip esp32s3 --port " + FLASH_PORT + " --partition-table firmware/partitions.csv --after hard-reset " + FIRMWARE_PATH

export ESP_LOG := "info"

# Cargo's per-crate compile chatter is suppressed by default. When a build is
# misbehaving and the progress lines matter, put it back for one invocation:
#   CARGO_TERM_QUIET=false just build
# Warnings and errors are unaffected; quiet only drops the progress output.
export CARGO_TERM_QUIET := env('CARGO_TERM_QUIET', "true")

# ...except for the test recipes, where quiet would also swallow the per-test
# names on a pass — the one place cargo's progress output earns its keep. An
# explicit CARGO_TERM_QUIET=1 still wins here, it only changes the default.
TEST_QUIET := env('CARGO_TERM_QUIET', "false")

# Host crates are pure and test natively, so reset the environment vars.
RESET_ENV := "
    unset LIBCLANG_PATH
    unset CARGO_BUILD_TARGET
    unset RUSTUP_TOOLCHAIN
    unset CARGO_UNSTABLE_BUILD_STD
    unset CARGO_UNSTABLE_BUILD_STD_FEATURES
"

_default:
    @just --list

[doc("Build the device firmware.")]
[group("deploy")]
build *ARGS:
    cargo build -p firmware --release {{ARGS}}

[doc("Build, flash, and open the serial monitor. Interactive: runs until you quit.")]
[group("deploy")]
flash *ARGS: (build ARGS)
    espflash flash --monitor {{FLASH_ARGS}}

# Flash without waiting for the app — for when the logs do not matter, or the
# build may never reach its boot marker. Never attaching the monitor is what
# makes this headless-safe: it is the monitor's input reader that wants a TTY,
# so `espflash flash` on its own runs clean from a script and exits 0.
[doc("Flash without attaching to the serial monitor.")]
[group("deploy")]
flash-only *ARGS: (build ARGS)
    espflash flash {{FLASH_ARGS}}

# Flash, stream the boot log, and exit as soon as the app reports MARKER —
# `espflash monitor` never terminates on its own, which makes it useless from a
# script. Fails fast (non-zero) on timeout or if the app gives up on the
# display. TIMEOUT is in seconds and doesn't include the build time.
[doc("Flash, stream the boot log, and exit as soon as the app reports MARKER.")]
[group("deploy")]
flash-log MARKER='boot: ready' TIMEOUT='20' TAIL='3': (build)
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
    set pid [spawn espflash flash --monitor {{FLASH_ARGS}}]
    expect {
        "{{MARKER}}" {
            # Keep reading for a moment. Exiting the instant the marker lands
            # hides whatever follows it — a panic one line later is invisible,
            # which is exactly how a BLE panic got missed once.
            set timeout {{TAIL}}
            expect {
                "PANIC" {
                    send_user "\n*** panicked after '{{MARKER}}'\n"
                    shutdown $pid 1
                }
                timeout {}
                eof {}
            }
            shutdown $pid 0
        }
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

# What the firmware is made of. `bloaty` reads the section table fine but
# refuses anything symbol-level on this target ("Unknown ELF machine value: 94"
# — 94 is xtensa), so `-d compileunits` and `-d symbols` are out. The esp
# toolchain's own `nm` has no such problem.
#
# The release profile sets `strip = "symbols"`, so the shipped binary has no
# symbol table at all; this overrides that for one build via the environment
# rather than editing the profile. It therefore relinks — expect a minute.
[doc("Show the top COUNT largest sections in the firmware binary.")]
[group("debug")]
size COUNT='25':
    #!/usr/bin/env bash
    set -euo pipefail
    CARGO_PROFILE_RELEASE_STRIP=none cargo build -p firmware --release 2>&1 \
        | grep -Ev '^(warning|  |$|note:)' || true
    bin=$(cargo metadata --format-version 1 --no-deps \
        | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')/xtensa-esp32s3-none-elf/release/firmware
    # Skip .debug_*: this build is unstripped, so debug info dwarfs everything
    # and none of it is flashed.
    echo "== sections (flashed + RAM; debug info omitted) =="
    xtensa-esp32s3-elf-size -A "$bin" \
        | awk '$2 ~ /^[0-9]+$/ && $2 > 0 && $1 !~ /^\.debug/ && $1 != "Total" {
                 total += $2; printf "%9d  %s\n", $2, $1 }
               END { printf "%9d  TOTAL\n", total }' \
        | sort -rn
    echo
    echo "== {{COUNT}} largest crates (approximates bloaty -d compileunits) =="
    # Buckets a demangled symbol by its leading path segment. Handles the two
    # shapes rustc emits: `crate::path::item` and `<crate::Type as Trait>::item`.
    xtensa-esp32s3-elf-nm --print-size --size-sort --radix=d -C "$bin" \
        | awk 'toupper($3) ~ /^[TRDB]$/ && $2 + 0 > 0 {
                 size = $2 + 0; $1=$2=$3=""; sub(/^ +/, ""); name = $0
                 gsub(/^[<&*(]+/, "", name)
                 crate = match(name, /^[A-Za-z_][A-Za-z0-9_]*::/) \
                       ? substr(name, 1, RLENGTH - 2) : "(C / asm / no path)"
                 total[crate] += size }
               END { for (c in total) printf "%9d  %s\n", total[c], c }' \
        | sort -rn | awk 'NR <= {{COUNT}}'
    echo
    echo "== {{COUNT}} largest symbols =="
    # Column 3 is the symbol type; keep code and data. The positive-size test
    # drops the linker's own absolutes (`_rwtext_len` reports -12832).
    # `--size-sort` is ascending, so `tail` takes the largest without closing
    # the pipe early — `head` here would SIGPIPE `nm` and trip `pipefail`.
    xtensa-esp32s3-elf-nm --print-size --size-sort --radix=d -C "$bin" \
        | awk 'toupper($3) ~ /^[TRDB]$/ && $2 + 0 > 0 {
                 size = $2 + 0; $1=$2=$3=""; sub(/^ +/, "");
                 printf "%9d  %s\n", size, $0 }' \
        | tail -{{COUNT}} | sort -rn

# Serial monitor only (no flash).
[group("deploy")]
monitor:
    espflash monitor --port {{FLASH_PORT}}

# Everything but `firmware`, which is device-only. `ui` and `apps` are in scope
# here — they build natively once the device env vars are unset, which is what
# RESET_ENV is for. A multi-line comment would become the recipe's `just --list`
# blurb (just takes the last line), so the summary goes in [doc] instead.
[doc("Host-side unit tests for our pure crates.")]
[group("verification")]
test *ARGS:
    #!/bin/sh
    set -e
    {{RESET_ENV}}
    CARGO_TERM_QUIET={{TEST_QUIET}} cargo test --workspace --exclude firmware {{ARGS}}

# Uses their workspace's default-members, which already excludes their
# device-only crates.
[doc("Host tests for the vendored upstream crates.")]
[group("verification")]
test-vendor *ARGS:
    #!/bin/sh
    set -e
    {{RESET_ENV}}
    CARGO_TERM_QUIET={{TEST_QUIET}} cargo test --manifest-path vendor/rust-enc/Cargo.toml {{ARGS}}

# `ui` is excluded because Slint's generated code trips a pile of pedantic
# lints we do not control — same reason ui/Cargo.toml drops `[lints] workspace`.
[doc("Clippy on our pure crates (host target, warnings are errors).")]
[group("verification")]
lint *ARGS:
    #!/bin/sh
    set -e
    {{RESET_ENV}}
    cargo clippy --workspace --exclude firmware --exclude ui --all-targets {{ARGS}} -- -D warnings

# No `--all-targets`: that adds the bin's implicit test harness, which wants
# libtest, which does not exist for a no_std xtensa target — the recipe fails
# with "can't find crate for `test`" before it lints anything. Firmware has no
# tests of its own anyway; they live in the pure crates, covered by `just lint`.
[doc("Clippy on the device firmware.")]
[group("verification")]
lint-device *ARGS:
    cargo clippy -p firmware --release {{ARGS}} -- -D warnings

[group("verification")]
fmt:
    cargo fmt --all

[group("verification")]
fmt-check:
    cargo fmt --all -- --check

# Everything CI would run.
[group("verification")]
check: fmt-check lint test build

[doc("Generates documentation for a given package in Markdown.")]
[group("debug")]
doc PACKAGE:
    #!/bin/sh
    set -e
    RUSTDOCFLAGS="-Z unstable-options --output-format json" cargo doc --release --no-deps --package {{PACKAGE}}
    CRATE=$(echo {{PACKAGE}} | tr '-' '_')
    rustdoc-md --path target/xtensa-esp32s3-none-elf/doc/${CRATE}.json --output target/xtensa-esp32s3-none-elf/doc/${CRATE}.md
    mkdir -p target/doc
    ln -sf ../xtensa-esp32s3-none-elf/doc/${CRATE}.md target/doc/${CRATE}.md
    echo "Documentation written to target/doc/${CRATE}.md"

# The device environment is this Justfile's default (see the exports at the
# top), so this recipe only has to hand the command that environment — no
# exports of its own. For anything cargo-adjacent that has no recipe here:
#   just exec-device cargo expand -p firmware
#   just exec-device cargo tree -i some-crate
# Also worth starting long-lived tools under, so their rust-analyzer inherits
# the device target instead of guessing: `just exec-device claude`.
[doc("Run any command with the device toolchain environment.")]
[group("debug")]
exec-device *COMMAND:
    #!/bin/sh
    {{COMMAND}}

# Prints the path and size of the compiled binary.
[group("debug")]
binary-info:
    #!/bin/sh
    TARGET=$(cargo build -p firmware --release --message-format=json-render-diagnostics | jq -r 'select(.reason == "compiler-artifact" and .target.kind[] == "bin") | .filenames[]')
    du -h $TARGET

# Print the toolchain paths this Justfile derived, for debugging.
[group("debug")]
env-info:
    @echo "LIBCLANG_PATH = $LIBCLANG_PATH"
    @echo "xtensa bin    = {{xtensa_bin}}"
    @rustc --version
