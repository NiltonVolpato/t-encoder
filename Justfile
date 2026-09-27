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

# Interactive mode for flashing and monitoring (defaults to true; set INTERACTIVE=false for automated/CI runs)
export INTERACTIVE := env('INTERACTIVE', "true")

_default:
    @just --list

# -----------------------------------------------------------------------------
# Portable Workspace Recipes (pure crates, apps, tests)
# -----------------------------------------------------------------------------

[doc("Run host unit tests for apps and pure crates in portable workspace.")]
[group("verification")]
test *ARGS:
    cd portable && cargo test --workspace {{ARGS}}

[doc("Clippy on portable workspace crates (warnings are errors).")]
[group("verification")]
lint *ARGS:
    cd portable && cargo clippy --workspace --all-targets {{ARGS}} -- -D warnings

[doc("Typecheck portable workspace crates.")]
[group("verification")]
check-native *ARGS:
    cd portable && cargo check --workspace --all-targets {{ARGS}}

[doc("Format all Rust files in both workspaces.")]
[group("verification")]
fmt:
    cd portable && cargo +nightly fmt --all
    cd esp32-devices && cargo +nightly fmt --all

[doc("Check formatting without modifying files.")]
[group("verification")]
fmt-check:
    cd portable && cargo +nightly fmt --all -- --check
    cd esp32-devices && cargo +nightly fmt --all -- --check

# -----------------------------------------------------------------------------
# Apps & Simulators (portable apps with Slint MCP)
# -----------------------------------------------------------------------------

[doc("Run a portable app simulator with Slint MCP enabled (e.g. just test-app clock, default port: 3450).")]
[group("apps")]
test-app APP="app-clock" PORT="3450" *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    pkg="{{APP}}"
    if [[ ! "$pkg" =~ ^app- ]] && [[ -d "portable/apps/$pkg" ]]; then
        pkg="app-$pkg"
    fi
    echo "Starting simulator for $pkg on Slint MCP port {{PORT}}..."
    cd portable && SLINT_EMIT_DEBUG_INFO=1 SLINT_MCP_PORT="{{PORT}}" cargo run -p "$pkg" {{ARGS}}

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

[doc("Attach to a running device and monitor logs without reflashing.")]
[group("device")]
attach TARGET=DEVICE *ARGS:
    probe-rs attach --chip=esp32s3 --always-print-stacktrace --log-format '{L:severity:bold:<1} {t:dimmed}] {s}' esp32-devices/target/xtensa-esp32s3-none-elf/release/{{TARGET}} {{ARGS}}

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

[doc("Show firmware RAM usage breakdown by sections and symbols using bloaty (default: waveshare_knob_1_8).")]
[group("device")]
ram TARGET=DEVICE COUNT="30":
    bloaty esp32-devices/target/xtensa-esp32s3-none-elf/release/{{TARGET}} \
        -d sections,symbols \
        --source-filter='^\.(bss|data|rwdata|rwtext|dram)' \
        -s vm --domain=vm -n {{COUNT}}

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

# -----------------------------------------------------------------------------
# Co-Processor Recipes (ESP32-U4WDH Wi-Fi/BT)
# -----------------------------------------------------------------------------

[doc("Build co-processor firmware with PlatformIO.")]
[group("coprocessor")]
build-coprocessor *ARGS:
    cd coprocessor && \
        pio run {{ARGS}}

[doc("Generate compile_commands.json for clangd and Zed IDE integration.")]
[group("coprocessor")]
compiledb:
    cd coprocessor && pio run -t compiledb

[private]
_monitor-coprocessor-expect PORT="/dev/cu.usbserial-10" TIMEOUT='15' TAIL='5':
    #!/usr/bin/env expect -f
    proc shutdown {pid code} {
        global spawn_id
        catch { send \003 }
        expect -timeout 5 {
            eof {}
            timeout {
                catch { exec kill -TERM -$pid }
                catch { expect -timeout 3 eof }
            }
        }
        catch { wait }
        exit $code
    }

    set timeout {{TIMEOUT}}
    set pid [spawn pio device monitor -d coprocessor --port {{PORT}} --baud 115200]
    expect {
        "Co-Processor initialized" {
            # Device initialized: keep streaming for TAIL seconds to catch post-boot panics, crashes, or bootloops
            set timeout {{TAIL}}
            expect {
                -re "(Guru Meditation Error|abort\\(\\)|assert failed|Backtrace:)" {
                    send_user "\n*** Panic/crash detected after initialization!\n"
                    shutdown $pid 1
                }
                timeout {
                    send_user "\n=== Co-processor initialized and stable ===\n"
                    shutdown $pid 0
                }
                eof {
                    send_user "\n*** Serial port closed unexpectedly\n"
                    shutdown $pid 1
                }
            }
        }
        -re "(Guru Meditation Error|abort\\(\\)|assert failed|Backtrace:)" {
            send_user "\n*** Panic/crash detected during boot!\n"
            shutdown $pid 1
        }
        timeout {
            send_user "\n*** Timed out waiting for co-processor initialization ({{TIMEOUT}}s)\n"
            shutdown $pid 1
        }
        eof {
            send_user "\n*** Serial monitor exited before initialization\n"
            exit 1
        }
    }

[doc("Flash co-processor firmware via CH340 and monitor output (interactive, or automated via INTERACTIVE=false).")]
[group("coprocessor")]
flash-coprocessor PORT="/dev/cu.usbserial-10" *ARGS:
    cd coprocessor && pio run -t upload --upload-port {{PORT}} {{ARGS}}
    if [ "$INTERACTIVE" = "true" ]; then \
        cd coprocessor && pio device monitor --port {{PORT}} --baud 115200; \
    else \
        just _monitor-coprocessor-expect {{PORT}}; \
    fi

[doc("Monitor co-processor UART output via CH340 (interactive, or automated via INTERACTIVE=false).")]
[group("coprocessor")]
monitor-coprocessor PORT="/dev/cu.usbserial-10" *ARGS:
    if [ "$INTERACTIVE" = "true" ]; then \
        cd coprocessor && pio device monitor --port {{PORT}} --baud 115200 {{ARGS}}; \
    else \
        just _monitor-coprocessor-expect {{PORT}}; \
    fi

[doc("Reset the co-processor into normal running mode without attaching.")]
[group("coprocessor")]
reset-coprocessor PORT="/dev/cu.usbserial-10":
    esptool -p {{PORT}} run

[doc("Run clang-tidy on co-processor C++ sources.")]
[group("coprocessor")]
tidy-coprocessor *ARGS:
    @cd coprocessor && PATH="/opt/homebrew/opt/llvm@22/bin:$PATH" /opt/homebrew/opt/llvm@22/bin/run-clang-tidy \
        -clang-tidy-binary=/opt/homebrew/opt/llvm@22/bin/clang-tidy \
        -clang-apply-replacements-binary=/opt/homebrew/opt/llvm@22/bin/clang-apply-replacements \
        -p . \
        -quiet \
        -hide-progress \
        -source-filter '.*/coprocessor/src/.*' \
        -header-filter '^(coprocessor/)?src/.*\.h$' \
        -removed-arg='-mlongcalls' \
        -removed-arg='-fno-shrink-wrap' \
        -removed-arg='-fno-tree-switch-conversion' \
        -removed-arg='-fstrict-volatile-bitfields' \
        -extra-arg='--target=xtensa-esp-elf' \
        -extra-arg='-Qunused-arguments' \
        -extra-arg='-Wno-error' \
        -extra-arg='--sysroot=/Users/nilton/.platformio/packages/toolchain-xtensa-esp-elf/xtensa-esp-elf' \
        -extra-arg='-isystem/Users/nilton/.platformio/packages/toolchain-xtensa-esp-elf/picolibc/include' \
        -extra-arg='-isystem/Users/nilton/.platformio/packages/toolchain-xtensa-esp-elf/xtensa-esp-elf/include/c++/15.2.0' \
        -extra-arg='-isystem/Users/nilton/.platformio/packages/toolchain-xtensa-esp-elf/xtensa-esp-elf/include/c++/15.2.0/xtensa-esp-elf/esp32' \
        -extra-arg='-isystem/Users/nilton/.platformio/packages/toolchain-xtensa-esp-elf/lib/gcc/xtensa-esp-elf/15.2.0/include' \
        -extra-arg='-isystem/Users/nilton/.platformio/packages/toolchain-xtensa-esp-elf/xtensa-esp-elf/include' \
        {{ARGS}}
