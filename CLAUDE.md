# t-encoder — LilyGo T-Encoder-Pro app launcher

Pure-Rust, `no_std`, Embassy-async firmware. A launcher hosting multiple apps,
added over time. Architecture plan: `../plans/architecture.md` (phases + checkboxes).

## Hardware (ESP32-S3-R8)

- 16 MB flash, **8 MB OCTAL/OPI PSRAM** (never quad — a quad config fails the
  PSRAM smoke test at boot).
- Display: **CO5300 AMOLED, 390×390 round, RGB565, QSPI**. Framebuffer ≈ 304 KB
  in PSRAM. SDIO0=11 SDIO1=13 SDIO2=7 SDIO3=14 SCLK=12 CS=10 RST=4 EN=3.
- Touch: **CHSC5816 @ I2C `0x2E`** (SDA=5 SCL=6, INT=9 RST=8). Note upstream's
  docs claim CST816 @ `0x15` — wrong for this unit; confirmed by observation.
- Encoder: A=1, B=2, button=0 (**GPIO0 is a boot strap pin** — held at reset
  enters ROM download mode).
- Buzzer/haptic: GPIO17 LEDC PWM. ~1.1 kHz = audible beep, ~200 Hz = felt buzz.
- Port: `/dev/cu.usbmodem101` (native USB-Serial/JTAG).

## Build / flash / test — always via `just`

```sh
just build          # device firmware (release)
just flash          # build + flash + monitor
just test           # host tests for our pure crates
just test-vendor    # host tests for the vendored upstream crates
just lint           # clippy on pure crates, warnings are errors
just check          # typecheck pure crates + device firmware
just verify         # fmt-check + lint + test + build (what CI runs)
just qemu           # boot a radio-less build under QEMU (see below)
```

`just env-info` prints the derived toolchain paths. There is no `cargo-esp`
wrapper — the Justfile derives `LIBCLANG_PATH` and the xtensa-gcc `PATH` with
globs, so toolchain upgrades need no edit here.

`just exec-device <cmd>` runs anything under the device environment, for the
cargo-adjacent tools that have no recipe (`cargo expand`, `cargo tree`). Long-
lived tools are worth starting this way too — `just exec-device claude` — so
their rust-analyzer inherits the device target rather than guessing at it.

`rust-analyzer.toml` is read but only partly obeyed. The server loads it (its
log shows `updating ra-toml workspace config` with no validation errors) and
then ignores `check.overrideCommand`, `check.allTargets` and `cargo.*`, which
are client-scoped settings that only an LSP client's `initializationOptions`
can supply. That is why `.zed/settings.json` exists; Claude Code exposes no
such hook, so its rust-analyzer runs the stock `cargo check … --keep-going
--all-targets` no matter what the toml says — confirmed by logging what it
spawns. Two classes of phantom diagnostic follow. Neither is real
breakage — `just check` and `just lint-device` are the source of truth — and
neither should be "fixed" in the source, least of all by gating test modules on
`target_os`:

- from *rustc*: `can't find crate for 'test'`, "`#[panic_handler]` required",
  "no global memory allocator", against the test targets of `launcher` /
  `apps` / `ui` / `firmware`. Flycheck keeps rust-analyzer's `allTargets`
  default, and `--all-targets` cannot succeed on a bare-metal target.
- from *rust-analyzer* itself: `unresolved extern crate` on `extern crate
  alloc`, `cannot apply unary operator !` on any `!bool`, and `None` reported
  as a non-snake-case *variable*. All one cause — RA has no `core` in its crate
  graph for xtensa, so the prelude is missing. Beware that hover can still look
  healthy here: primitives like `u16` and local modules resolve without `core`.

Cargo's progress output is suppressed by default (`CARGO_TERM_QUIET`); set
`CARGO_TERM_QUIET=false` for one invocation when a build is misbehaving. The
test recipes default the other way, since quiet also hides per-test names.

### Critical build invariants — do not "simplify" these

- **The device target and build-std live in the Justfile as environment
  variables, not in `.cargo/config.toml`.** A cargo config setting can be
  appended to but never unset, and there is no value of `[build] target` that
  means "build natively" — so with the target pinned in the config file, host
  test and clippy runs cannot escape xtensa. `[unstable] build-std` there has
  the same problem in reverse: applied globally, it breaks host builds with
  duplicate lang items vs the real `std`. So the Justfile exports
  `CARGO_BUILD_TARGET` and `CARGO_UNSTABLE_BUILD_STD{,_FEATURES}` by default,
  and the host recipes (`test`, `test-vendor`, `lint`) `unset` them via
  `RESET_ENV`. The root config carries no target at all.
- The esp toolchain ships **no precompiled `core`** for
  `xtensa-esp32s3-none-elf`, which is why build-std is required at all — and why
  `rust-analyzer.toml` must pass the same flags in its `overrideCommand`s.
  Deleting it makes RA report "can't find crate for `core`" on every file.
  `.zed/settings.json` repeats the same commands because a client's
  `initialization_options` win over `rust-analyzer.toml`; keep the two in sync.
- **`[profile.release]` must live in the root `Cargo.toml`.** The submodule's
  own profile section is ignored (only root workspace profiles apply); without
  ours we silently ship a bloated, slow build.
- Flashing without `--partition-table firmware/partitions.csv` silently disables
  settings persistence (the custom `settings` NVS partition goes missing).
- **`--after hard-reset` is required.** Without it espflash leaves the chip in
  ROM download mode ("waiting for download") with a black screen — no app is
  running, so nothing initializes the display. Recover with `espflash reset`.
- **`firmware`'s default features are load-bearing.** `default = ["radio",
  "float-save-restore"]` is what a real build needs; `--no-default-features` is
  the QEMU build and nothing else. That is also why the root `Cargo.toml` takes
  esp-hal with `default-features = false` and lists `rt` / `exception-handler`
  by hand, and why `esp-rtos` does *not* list `esp-radio` there — both would
  otherwise be unconditional and unremovable. Features are additive: anything
  that must be *absent* from one build cannot be requested by any crate in the
  graph, which is the whole reason `enc-net`, `esp-radio`, `bt-hci` and
  `trouble-host` are optional dependencies.

### espflash behaviour worth knowing

- **`espflash flash` does not erase the whole chip and does not repartition.**
  It writes only the offsets that changed, so the `settings` and `nvs` data
  partitions survive a reflash. Full erase is opt-in and explicit:
  `erase-flash`, `erase-parts`, `erase-data-parts`. (Verified on device.)
- **It is the *monitor* that needs a TTY, not flashing.** `espflash flash`
  without `--monitor` runs clean headlessly and exits 0 — that is what `just
  flash-only` relies on (verified on device). Add `--monitor` and the input
  reader wants a terminal: headless it either dies with "Failed to initialize
  input reader" or hangs silently. Wrap those in `script -q /dev/null <cmd>`.
  `read-flash` / `write-bin` hang even then — do not use them from automation.
- **Mid-execution monitoring does not work with espflash** on this board.
  `monitor` always tries to sync with the bootloader, and the app is not one, so
  `--before no-reset` and `--before no-reset-no-sync` both hang at
  "Connecting...". Unlike boards with an external USB-UART bridge (CP2102/CH340)
  that stays enumerated regardless, this board uses the ESP's *own*
  USB-Serial/JTAG, which re-enumerates across reset. To watch a running app, use
  a dumb terminal (`screen /dev/cu.usbmodem101 115200`) — `esp-println` output
  reads fine; it is espflash's handshake that is the blocker.
- Only one process may hold `/dev/cu.usbmodem101`. A stray `cat` or a background
  monitor blocks every espflash invocation, usually as an unexplained hang.

### QEMU — boot, PSRAM and settings, then the first missing peripheral

```sh
just qemu [TIMEOUT]   # boot under Espressif's qemu-xtensa, quit after N seconds
just qemu-gdb [PORT]  # same, frozen at reset with a gdb stub
just gdb [PORT]       # attach xtensa-esp32s3-elf-gdb to that stub
```

**`just qemu` does not run the binary `just build` produces.** It builds and
patches its own, because three things have to be true before the app gets past
`esp_hal::init` — all three verified under gdb:

1. **No radio.** `--no-default-features` drops firmware's `radio` feature (see
   `firmware/src/radio.rs`). QEMU emulates neither Wi-Fi nor BT.
2. **No FPU, anywhere.** The S3 has a real single-precision FPU (`rustc --print
   cfg --target xtensa-esp32s3-none-elf` lists `target_feature="fp"`; there is
   no `dfp`, so `f64` is soft either way). QEMU's core model has none, and meets
   one by **segfaulting qemu-system-xtensa itself** (exit 139) — `rur.fcr` in
   xtensa-lx-rt's `save_context`, on the first exception. Two halves to the fix:
   `--no-default-features` drops `float-save-restore` (hence esp-hal with
   `default-features = false` in the root `Cargo.toml`), and the recipe's
   `QEMU_RUSTFLAGS` passes `-C target-feature=-fp` so the app's own float code —
   Slint's renderer above all — lowers to compiler-builtins calls instead.
3. **The BBPLL calibration wait patched out of the ELF**
   (`firmware/qemu-patch.py`). `esp_hal::init` otherwise spins forever in
   `enable_pll_clk_impl` on `while
   I2C_ANA_MST.ana_conf0().bbpll_cal_done().bit_is_clear() {}`: QEMU models no
   I2C_ANA_MST (analog/PLL, 0x6000_E000), so that window reads back 0 and drops
   writes and the done bit never arrives. Every `CpuClock` preset routes through
   the PLL, so no `esp_hal::Config` escapes it. The patch goes on the ELF, not
   the image — the image carries a checksum and SHA256 that only
   `espflash save-image` recomputes.

What that buys, as of the last run: boot banner, persisted settings, **PSRAM up
and the octal smoke test passing**, heap figures — then `PANIC: ... Exception
occurred on ProCpu 'InstrProhibited' ... PC: 0`.

**That last one is the interrupt matrix, not a driver.** The chain, every step
of it read out of a live gdb session:

1. `esp_rtos::start(timg0.timer0, …)` binds **TG0_T0_LEVEL (source 50)** to CPU
   interrupt 1. Timer groups QEMU does model, and the tick duly fires — at the
   fault, the `interrupt` SR reads `0x2`, i.e. CPU interrupt 1. So delivery and
   routing both work.
2. esp-hal's `handle_interrupts::<1>` then asks the matrix *which source* it
   was, via `InterruptStatus::current()`. All four `core_0_intr_status` words at
   0x600c_218c read `0x00000006` — the same value at reset, before a single
   guest instruction. A live pending mask cannot be a constant, and source 50's
   bit (word 1, bit 18) is not among them.
3. So it iterates a fabricated set, settles on **source 33** — which is not even
   a named interrupt on the S3; the PAC's `Interrupt` enum jumps straight from
   `MCPWM1 = 32` to `LEDC = 35` — and calls `__INTERRUPTS[33]._handler`.
4. That slot is null. `PC := 0`, `InstrProhibited`.

CPU-*internal* interrupts take a different path — a hardcoded match on the CPU
interrupt number — and work fine; `Software0` is handled twice before this. It is
specifically the first *peripheral* interrupt that is fatal.

**Why Espressif's own examples run here and ours does not.** Not because QEMU is
broken for everyone — because IDF never reads that register. Its xtensa dispatch
(`components/xtensa/xtensa_vectors.S:328`) picks the highest set bit of the
`INTERRUPT` special register and indexes a 32-entry table **by CPU interrupt
number**:

```asm
find_ms_setbit a3, a4, a3, 0     ; a3 = CPU interrupt number
movi    a4, _xt_interrupt_table
addx8   a3, a3, a4
l32i    a4, a3, XIE_HANDLER
callx4  a4
```

Grep `esp_hw_support/`, `xtensa/` and `hal/` for `intr_status` and the only hits
are PMU sleep code. IDF uses the matrix once, at `esp_intr_alloc` time, to route
a source to a CPU interrupt; after that the status register is irrelevant to it.
esp-hal instead re-reads the 99-bit source bitmap on every interrupt and
dispatches by source. So an unreliable status register is invisible to IDF and
fatal to us. Note esp-hal's RISC-V path (`interrupt/riscv.rs:563`) does the same
thing, so this is not an xtensa-only exposure.

Nothing above it is a driver problem: under gdb the buzzer task is spawned and
parked in its channel receive before this fires, and `display::init` is never
reached. Nor is a stub handler the fix — nop-ing the dispatch does clear the
panic, and the app then livelocks re-entering `__level_1_interrupt`, because
TG0_T0 is level-triggered and only its real handler would acknowledge it.

Matching every `idf.py qemu` argument (see the Justfile) does **not** fix it, and
neither does the eFuse image fix the BBPLL wait. Both are worth having anyway,
for a machine state Espressif actually tests. This is where the investigation
stopped.

That is roughly the ceiling. Per Espressif's own matrix, the esp32s3 machine
models flash + MMU, eFuse, RNG, GDMA, SysTimer, timer groups, the crypto blocks
and **PSRAM (QPI and OPI)** — but *not* LEDC, GP SPI, I2C, RMT, USB, the GPIO
matrix/IOMUX, or either radio. So the display (QSPI), touch (I2C), encoder
(PCNT) and buzzer (LEDC) are all out of reach. `launcher`/`ui`/`apps` host tests
cover that half far better; QEMU is for the boot path.

The QEMU arguments mirror `idf.py qemu`
(`esp-idf/tools/idf_py_actions/qemu_ext.py`) — eFuse backing store, timer-group
`wdt_disable`, `open_eth` NIC — since that is the only configuration Espressif
tests this machine in. Two deliberate divergences, both commented in the
Justfile: `-m 8M` rather than IDF's `-m 32M`, because 32 MiB makes esp-hal's
PSRAM mapping fail outright (`cache_dbus_mmu_set failed`); and `-monitor none`,
because a (qemu) prompt beside a gdb session desynchronises the two.

PSRAM is opt-in and both halves matter: `-m 8M` sizes it and
`-global driver=ssi_psram,property=is_octal,value=true` selects OPI to match the
ESP32-S3-R8. Miss either and `heap::smoke_test` fails.

#### Debugging it

```sh
just qemu-gdb          # terminal 1 — frozen at reset, waits for gdb
just gdb               # terminal 2
(gdb) hbreak firmware_panic_stop
(gdb) continue
```

`firmware_panic_stop` exists only to be a fixed symbol to break on: every panic
comes to rest there, and the backtrace runs back through the exception handler
to the faulting frame. `#[panic_handler]` ignores `export_name`, Rust's own
`rust_begin_unwind` carries a per-build hash, and a `break` instruction is no
help either — esp-hal's exception handler catches the debug exception itself and
panics "Breakpoint on ProCpu" rather than letting it reach the stub.

Four traps, each of which will cost an afternoon:

- **`continue` goes in gdb, never in a qemu monitor.** Resuming from the monitor
  leaves gdb thinking the target is halted and it then ignores the guest
  entirely. `just qemu-gdb` passes `-monitor none` so there is no (qemu) prompt.
- **`hbreak`, not `break`** — the code is in flash-mapped `.text`, which QEMU
  will not let gdb write, so software breakpoints silently never fire. Two
  hardware breakpoints work; three have hung the stub.
- **`interrupt` / Ctrl-C does not stop this stub.** Everything is
  breakpoint-driven; that is what `-S` (freeze at reset) is for.
- `0x40000400 in ?? ()` on connect is *correct* — the ROM reset vector, before
  any of our code. A one-frame `bt` with no symbols there is not a failure.

Console traps: the app's serial goes to **UART0**, not USB-Serial/JTAG (QEMU has
no S3 USB-Serial/JTAG device; esp-println's `auto` printer probes for one, reads
0 and falls back), and the chardev must be `-serial file:/dev/stdout` — `-serial
stdio` wants a terminal and prints nothing at all when run headless. A guest
wedged on an unmodelled peripheral can also stop servicing SIGTERM, so the
recipe's `timeout` carries `-k`.

Docs: <https://github.com/espressif/esp-toolchain-docs/blob/main/qemu/README.md>
(feature matrix) and `qemu/esp32s3/README.md` (flags).

## Layout

| Crate | Target | Responsibility |
|---|---|---|
| `launcher` | host | **Pure.** `App` trait, router, layout, animation. No hardware. |
| `firmware` | xtensa | Device binary: boot, task spawn, hardware glue, main loop |
| `vendor/rust-enc` | submodule | Upstream drivers — **not** a workspace member |

`vendor/rust-enc` is a pinned submodule of `NiltonVolpato/rust-enc` (fork of
`rayslava/rust-enc`, MIT). It is `exclude`d from our workspace and keeps its own
`[workspace]` root so its crates' `workspace = true` inheritance resolves; we
reach it by path dependency. Updates are explicit — never automatic.

`firmware/src/{display,touch,buzzer,input,heap,settings}.rs` are **copied** from
upstream's `enc-app` (they live in a binary crate and are unreachable by path
dependency). They have diverged from upstream by definition; do not expect to
pull fixes into them automatically.

Workspace dependency versions must stay semver-compatible with
`vendor/rust-enc/Cargo.toml`'s `[workspace.dependencies]`. A mismatch builds two
copies of `esp-hal` and fails at link time. Bump both together, deliberately.

## Rust rules

1. Files **< 500 lines**; modules atomic; tests in separate files/modules.
2. **clippy pedantic, zero warnings.** No `#[allow(...)]`; fix or delete.
3. **No `unwrap()`/`expect()`/`panic!()` in production paths** (tests fine).
   Propagate with `?` and typed error enums.
4. No `#[ignore]`d or excluded tests.
5. No needless clones/allocations. `Box`/`dyn` are avoided by default — the
   `App` trait is the one deliberate, documented exception (see the plan).
6. Minimize dependencies; the esp ecosystem needs coordinated pins.
7. Idiomatic, statement-oriented Rust: pattern matching, typed enums, errors
   over codes.
8. Comments and rustdoc short and to the point.

### Concurrency

Single core through P3. Two constraints keep a later dual-core split cheap:

- **`App::render(&self, ...)` — never `&mut self`.** Render must not mutate.
- Cross-app shared state lives in atomics (the `enc-state` pattern), never in
  app structs.

`esp-rtos` 0.3 has real SMP (`start_second_core`), but it is one SMP scheduler
across both cores, not two isolated Embassy stacks. When we do use core 1, the
first tenant is the **display flush pipeline**, not app logic — see the plan.

## Post-change pipeline

```sh
just fmt && just lint && just test && just build
```

Fix warnings in modules you touch. No unrequested side-quests.
