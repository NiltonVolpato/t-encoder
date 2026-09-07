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
just check          # fmt-check + lint + test + build (what CI runs)
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
