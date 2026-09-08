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

# QEMU-related variables. Espressif's fork ships inside the esp-idf tool tree;
# glob for it so a tool upgrade needs no edit here, same as the xtensa toolchain
# above. No `size=` on the drive: `save-image --merge` already emits a full
# 16 MiB chip, so pinning the size again just duplicated a fact that can drift.
QEMU_BIN := `ls -d ~/.espressif/tools/qemu-xtensa/*/qemu/bin/qemu-system-xtensa 2>/dev/null | head -1`
QEMU_ELF := justfile_directory() + "/target/qemu-firmware.elf"
QEMU_IMAGE := justfile_directory() + "/target/qemu-flash.bin"
QEMU_EFUSE := justfile_directory() + "/target/qemu-efuse.bin"

# The argument set below mirrors what `idf.py qemu` passes
# (esp-idf/tools/idf_py_actions/qemu_ext.py), which is the only configuration
# Espressif actually tests this machine in. One deliberate divergence: IDF hard-
# codes `-m 32M` and lets the app size PSRAM from its own sdkconfig, but 32 MiB
# makes esp-hal's mapping give up — `cache_dbus_mmu_set failed`, psram/esp32s3.rs
# — so we pass the 8 MiB the ESP32-S3-R8 actually has. `is_octal` to match it.
QEMU_PSRAM := "-m 8M -global driver=ssi_psram,property=is_octal,value=true"

# Backing store for the eFuse block. Without it every eFuse read returns zero:
# the boot log says "chip revision: v0.0" and qemu complains "[Efuse] Out of
# range key block specified: 0". It matters because esp-hal reads eFuses on the
# clock path — `pvt_supported()` takes `block_version()`, `dig_dbias_v1()` takes
# K_DIG_LDO and V_DIG_DBIAS20 — so with no eFuses it calibrates against zeroes.
# With the image, the log reads "chip revision: v0.3". (It does not fix the BBPLL
# wait; that patch is still required.)
QEMU_EFUSE_ARGS := "-drive file=" + QEMU_EFUSE + ",if=none,format=raw,id=efuse -global driver=nvram.esp32s3.efuse,property=drive,value=efuse"

# Two more that IDF passes unconditionally. `wdt_disable` turns off the timer
# group watchdog — no observed effect on our boot, but Espressif disables it on
# every QEMU run and it costs nothing to match. `open_eth` is the virtual NIC;
# also inert here, since the QEMU build has no networking.
QEMU_QUIRKS := "-global driver=timer.esp32s3.timg,property=wdt_disable,value=true -nic user,model=open_eth"

# Soft float for the QEMU build. The S3 has a real single-precision FPU
# (`rustc --print cfg` lists target_feature="fp"); QEMU's core model does not,
# and meets one by taking the whole emulator down. Dropping `float-save-restore`
# is enough for the boot path, but Slint's renderer is float-heavy, so the app
# would hit the same wall the moment it drew anything. `-fp` lowers all of it to
# compiler-builtins calls: `add.s`/`mul.s`/`lsi` go from ~4750 to zero (what
# objdump still shows is literal pools decoded as instructions).
#
# The two link args are repeated here on purpose: RUSTFLAGS *replaces*
# `.cargo/config.toml`'s `target.*.rustflags` rather than appending, so leaving
# them out silently drops `-nostartfiles`. `-C target-feature` is unstable, so
# rustc prints a warning about `fp` on every build; that is expected.
QEMU_RUSTFLAGS := "-C target-feature=-fp -C link-arg=-nostartfiles -C link-arg=-Wl,--no-warn-rwx-segments"

# `-monitor none` is ours, not IDF's (they multiplex it onto stdio with
# `mon:stdio`). Keep it: a (qemu) prompt next to a gdb session is a trap, since
# resuming from the monitor leaves gdb convinced the target is still halted.
QEMU_ARGS := "-machine esp32s3 -nographic -monitor none " + QEMU_PSRAM + " " + QEMU_EFUSE_ARGS + " " + QEMU_QUIRKS + " -drive file=" + QEMU_IMAGE + ",format=raw,if=mtd"

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

# Boots the flash image under Espressif's qemu-xtensa. It gets through the ROM
# and the IDF second-stage bootloader, then runs the app as far as the first
# peripheral QEMU does not model. Today that is:
#
#   enc-app v0.1.0 boot OK
#   settings: loaded toggles=0x0000 alarm=None
#   psram: 8192 KiB mapped at 0x3c0a0000
#   psram: smoke test OK (octal mode confirmed)
#   radio: built without the `radio` feature — no Wi-Fi, no BLE
#   heap: internal free=204816 used=0 | psram free=8080312 used=0
#   PANIC: Exception occurred on ProCpu 'InstrProhibited' ... PC: 0
#
# That is the interrupt matrix, not a driver. `esp_rtos::start` binds
# TG0_T0_LEVEL (source 50) to CPU interrupt 1, the tick fires, and the CPU takes
# it correctly — but when esp-hal asks the matrix *which* source it was, all four
# `core_0_intr_status` words read 0x00000006, the same value they hold at reset
# before any guest code. Source 50's bit is not among them. So
# `handle_interrupts::<1>` settles on source 33 — not even a named interrupt on
# this chip — and calls `__INTERRUPTS[33]._handler`, which is null. PC := 0.
#
# CPU-*internal* interrupts use a hardcoded match and work fine (Software0 is
# handled twice first); it is the first *peripheral* interrupt that is fatal.
# Everything before it is real: the buzzer task is spawned and parked in its
# channel receive (verified under gdb), and `display::init` is never reached.
#
# Getting that far needs three things, and none of them is optional:
#
#  1. **No radio.** `--no-default-features` drops firmware's `radio` feature.
#     QEMU emulates neither Wi-Fi nor BT, and esp-radio would also drag in
#     `xtensa-lx-rt/float-save-restore` — see (2).
#  2. **No FPU, anywhere.** QEMU's esp32s3 core has none, and `rur.fcr` in
#     xtensa-lx-rt's `save_context` takes qemu-system-xtensa down with it on the
#     first exception (segfault, exit 139). `--no-default-features` drops
#     `float-save-restore`, and QEMU_RUSTFLAGS soft-floats the rest so the app's
#     own float code cannot hit the same wall later.
#  3. **The BBPLL calibration wait patched out of the ELF** — see
#     `firmware/qemu-patch.py`. QEMU models no I2C_ANA_MST, so esp-hal's
#     `enable_pll_clk_impl` waits forever for a calibration bit that never sets.
#     No `esp_hal::Config` avoids it; every CpuClock preset uses the PLL.
#
# So the binary under QEMU is NOT the binary that ships — `just qemu` builds its
# own and patches it. It is a boot/PSRAM/settings smoke test, not the app: the
# display (QSPI), touch (I2C), encoder (PCNT) and buzzer (LEDC) are all
# unmodelled, and so is the FPU that Slint's software renderer needs.
#
# PSRAM *is* modelled, opt-in: `-m 8M` sizes it and the ssi_psram global selects
# octal, matching the ESP32-S3-R8. Without both, the smoke test fails.
#
# UART0 is the right console even though the device uses USB-Serial/JTAG: QEMU
# has no esp32s3 USB-Serial/JTAG device, and esp-println's default `auto` printer
# probes for one, reads 0, and falls back to UART0. It is written as
# `file:/dev/stdout`, not `stdio`, because qemu's stdio chardev wants a terminal
# — headless it emits nothing at all, silently (same trap as espflash's monitor).
[doc("Boot a radio-less, QEMU-patched build under QEMU. TIMEOUT seconds, then quit.")]
[group("debug")]
qemu TIMEOUT='15':
    #!/usr/bin/env bash
    set -euo pipefail
    just _qemu-build
    set +e
    # The timeout is the expected exit path, and qemu announces the SIGTERM on
    # stderr; drop that one line rather than let it read as a failure. `-k` is
    # not belt-and-braces: a guest wedged on an unmodelled peripheral has been
    # seen to stop servicing SIGTERM entirely, and only SIGKILL ends it.
    timeout -f -k 5s {{TIMEOUT}}s "{{QEMU_BIN}}" {{QEMU_ARGS}} -serial file:/dev/stdout 2>&1 \
        | grep -v 'terminating on signal 15'
    exit 0

# Same image, frozen at the first instruction with the gdb stub listening.
# Attach from another shell:
#
#   just qemu-gdb                      # terminal 1, waits for gdb
#   just gdb                           # terminal 2
#
# Then, in gdb:
#
#   hbreak firmware_panic_stop    # every panic comes to rest here
#   continue                      # <- IN GDB. see below
#
# Three things that will otherwise waste an afternoon:
#
#  - **`continue` belongs in gdb, never in a qemu monitor.** Resuming from the
#    monitor leaves gdb believing the target is still halted, and it then
#    ignores everything the guest does. That is why this recipe passes
#    `-monitor none`: there is no (qemu) prompt to be tempted by.
#  - **`hbreak`, not `break`.** The code is in flash-mapped `.text`, which QEMU
#    will not let gdb write, so software breakpoints silently never fire. Two
#    hardware breakpoints are reliable; three have hung the stub here.
#  - **`interrupt` / Ctrl-C does not stop this stub.** Everything has to be
#    breakpoint-driven, which is what `-S` is for.
#
# `0x40000400 in ?? ()` on connect is not a fault — that is the ROM reset
# vector, before any of our code, so `bt` having one frame and no symbols is
# correct. Serial output lands in target/qemu-serial.log.
[doc("Boot under QEMU frozen at reset with the gdb stub on PORT.")]
[group("debug")]
qemu-gdb PORT='3333':
    #!/usr/bin/env bash
    set -euo pipefail
    just _qemu-build
    echo "gdb stub on :{{PORT}} — attach with: just gdb {{PORT}}"
    "{{QEMU_BIN}}" {{QEMU_ARGS}} -serial file:target/qemu-serial.log -S -gdb tcp::{{PORT}}

[doc("Attach the xtensa gdb to a running `just qemu-gdb`.")]
[group("debug")]
gdb PORT='3333':
    #!/usr/bin/env bash
    set -euo pipefail
    gdb=$(ls -d ~/.espressif/tools/xtensa-esp-elf-gdb/*/xtensa-esp-elf-gdb/bin/xtensa-esp32s3-elf-gdb | head -1)
    exec "$gdb" -q -ex "target remote :{{PORT}}" {{QEMU_ELF}}

# The QEMU build: radio-less, unstripped (so `just gdb` has names), patched, and
# merged into one raw 16 MB chip for `if=mtd`. `--merge` because QEMU boots the
# whole flash rather than an app partition, so the bootloader and the partition
# table have to sit in the same file at their own offsets.
#
# Built into its own target dir: the feature set differs from `just build`, and
# sharing one would make the two recipes rebuild each other's work every time.
# The ELF is copied before patching so the patch never lands on a cargo output
# that a later build would reuse without rebuilding.
#
# Still espflash rather than IDF's `esptool merge-bin --pad-to-size`: both were
# tried against the same ELF and the emulator behaved identically, so the second
# tool would only add a step that splits espflash's output back into bootloader,
# partition table and app just to re-merge them.
_qemu-build: _qemu-efuse
    #!/usr/bin/env bash
    set -euo pipefail
    CARGO_PROFILE_RELEASE_STRIP=none RUSTFLAGS="{{QEMU_RUSTFLAGS}}" \
        cargo build -p firmware --release --no-default-features --target-dir target/qemu
    cp target/qemu/xtensa-esp32s3-none-elf/release/firmware {{QEMU_ELF}}
    python3 firmware/qemu-patch.py xtensa-esp32s3-elf-objdump {{QEMU_ELF}}
    espflash save-image --chip esp32s3 --partition-table firmware/partitions.csv \
        --flash-size 16mb --merge {{QEMU_ELF}} {{QEMU_IMAGE}}

# IDF's default esp32s3 eFuse image, reproduced rather than vendored as a blob:
# 1 KiB of zeroes with byte 38 = 0x0c, which is WAFER_VERSION_MINOR = 3, i.e.
# chip revision v0.3. Taken from QEMU_TARGETS['esp32s3'].default_efuse in
# esp-idf/tools/idf_py_actions/qemu_ext.py; that file also documents how to
# regenerate it with esptool/espefuse if the defaults ever move.
#
# QEMU writes back to this file, so it is left alone once created — delete it to
# get factory defaults again.
_qemu-efuse:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! -f "{{QEMU_EFUSE}}" ]]; then
        python3 -c "import pathlib, sys; b = bytearray(1024); b[38] = 0x0C; pathlib.Path(sys.argv[1]).write_bytes(bytes(b))" "{{QEMU_EFUSE}}"
        echo "wrote {{QEMU_EFUSE}} (chip revision v0.3)"
    fi

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
