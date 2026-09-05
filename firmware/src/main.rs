// Adapted from rust-enc (https://github.com/rayslava/rust-enc), MIT licensed.
// Copyright (c) rayslava. Copied from enc-app, which is a binary crate and so
// cannot be reached by path dependency; diverges from upstream from here on.

//! Device firmware entry point for the `LilyGo` T-Encoder-Pro (ESP32-S3).
//!
//! Brings up the hardware — CO5300 QSPI display over a PSRAM framebuffer, PCNT
//! encoder, CHSC5816 touch, buzzer, Wi-Fi — then hands the UI to
//! [`launcher::Router`]. This file owns no screens: encoder, button and touch
//! are normalized into `launcher::Input` and the router decides whether they
//! move the carousel or reach the active app.

#![no_std]
#![no_main]

extern crate alloc;

mod ble;
mod buzzer;
mod display;
mod heap;
mod input;
mod settings;
mod touch;

use buzzer::Feedback;
use embassy_executor::Spawner;
use embassy_time::{Duration, Instant, Timer};
use enc_input::Encoder;
use enc_state::{AppState, ConnState};
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::psram;
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
// `Input` is aliased to `UiInput`: esp-hal's GPIO `Input` already owns that name.
use launcher::{AppFactory, Ctx, Input as UiInput, Router, View};
// `as_weak` on the generated Slint component comes from this trait.
use slint::ComponentHandle;

esp_bootloader_esp_idf::esp_app_desc!();

/// Framebuffer size (mirrors `enc_config::display`); the panel's own width and
/// height live in `display::Display`, which does the clipping.
const DISPLAY_BYTES: usize = enc_config::display::FRAMEBUFFER_BYTES;
/// How long the button must be held before the long press fires.
const LONG_PRESS: Duration = Duration::from_millis(600);
/// Quadrature counts per mechanical detent (this encoder emits 2 per click).
const COUNTS_PER_DETENT: u8 = 2;
/// Log every one of the first this-many repaints, then one in
/// [`FRAME_LOG_EVERY`]. The first frames are the interesting ones; a periodic
/// sample after that shows the steady-state repaint rate without a 200 Hz loop
/// flooding a 64-byte serial FIFO.
const FRAME_LOG_FIRST: u32 = 5;
/// Sampling interval for repaint logging once past [`FRAME_LOG_FIRST`].
const FRAME_LOG_EVERY: u32 = 500;

/// Shared, lock-free app state mirrored between the UI loop and the Wi-Fi tasks.
static APP_STATE: AppState = AppState::new(1);

/// Device uptime in whole seconds (monotonic), clamped to `u32`. Feeds the
/// shared clock: current time = synced epoch + (uptime now − uptime at sync).
fn uptime_secs() -> u32 {
    u32::try_from(Instant::now().as_secs()).unwrap_or(0)
}

/// Device uptime in milliseconds, for the launcher's animation clock.
fn now_ms() -> u64 {
    Instant::now().as_millis()
}

/// Builds the Slint card model from the app registry, so the carousel is
/// driven by the same manifests the router uses — one source of truth.
fn app_cards(factories: &[&dyn AppFactory]) -> slint::ModelRc<ui::CardData> {
    let cards: alloc::vec::Vec<ui::CardData> = factories
        .iter()
        .map(|factory| {
            let manifest = factory.manifest();
            ui::CardData {
                name: manifest.name.into(),
                accent: rgb565_to_slint(manifest.accent),
                icon: i32::from(manifest.icon.0),
            }
        })
        .collect();
    slint::ModelRc::new(slint::VecModel::from(cards))
}

/// Widens an RGB565 colour to Slint's 8-bit-per-channel `Color`.
///
/// Each channel is scaled by its max rather than shifted, so full-scale stays
/// full-scale (a plain `<< 3` would cap red at 248 and never reach white).
fn rgb565_to_slint(color: embedded_graphics::pixelcolor::Rgb565) -> slint::Color {
    use embedded_graphics::prelude::RgbColor;
    let scale = |value: u8, max: u8| -> u8 {
        let widened = u16::from(value).saturating_mul(255) / u16::from(max).max(1);
        u8::try_from(widened).unwrap_or(0)
    };
    slint::Color::from_rgb_u8(
        scale(color.r(), 31),
        scale(color.g(), 63),
        scale(color.b(), 31),
    )
}

/// Required as a lang item, but **not reached under the current release
/// profile**: `panic = "immediate-abort"` compiles `panic!` straight to an
/// abort, and this string is not even in the binary (checked with `strings`).
/// It is kept for the debug profile and for whenever the trade is reversed —
/// see the note in the root `Cargo.toml`.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    esp_println::println!("PANIC: {info}");
    loop {
        core::hint::spin_loop();
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_println::logger::init_logger_from_env();

    // Internal-only global heap (esp_alloc::HEAP) — serves esp-radio + DMA.
    // Registered before esp_rtos::start / any radio use. PSRAM is a SEPARATE
    // non-global heap (see below); the radio never allocates from PSRAM. Two
    // regions: reclaimed bootloader RAM + a static DRAM array for coex headroom.
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: heap::INTERNAL_HEAP_RECLAIMED);
    esp_alloc::heap_allocator!(size: heap::INTERNAL_HEAP_EXTRA);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    log::info!("enc-app v{} boot OK", env!("CARGO_PKG_VERSION"));

    // Restore persisted settings (alarm + toggles) from flash into shared state.
    let saved = settings::load();
    APP_STATE.set_toggles(saved.toggles);
    if let Some(minute) = saved.alarm {
        APP_STATE.set_alarm(Some(minute));
    }
    log::info!(
        "settings: loaded toggles={:#06x} alarm={:?}",
        saved.toggles,
        saved.alarm
    );

    // ESP32-S3-R8 carries octal (OPI) PSRAM. Smoke test confirms it on hardware.
    let psram = psram::Psram::new(
        peripherals.PSRAM,
        psram::PsramConfig {
            mode: psram::PsramMode::OctalSpi,
            size: psram::PsramSize::AutoDetect,
            ram_frequency: psram::SpiRamFreq::Freq80m,
            ..Default::default()
        },
    );
    let (psram_start, psram_size) = psram.raw_parts();
    log::info!("psram: {} KiB mapped at {psram_start:p}", psram_size / 1024);
    let psram_ok = heap::smoke_test(psram_start, psram_size);
    if psram_ok {
        log::info!("psram: smoke test OK (octal mode confirmed)");
        // Separate (non-global) PSRAM heap for app bulk, past the framebuffer.
        if !heap::init_psram_heap(psram_start, psram_size, DISPLAY_BYTES, heap::PROBE_LEN) {
            log::error!("psram: heap region not registered (range invalid/too small)");
        }
    } else {
        log::error!("psram: smoke test FAILED — check PSRAM mode (octal vs quad)");
    }

    // Wi-Fi STA + embassy-net (Phase 6b). A random seed salts DHCP/ports; the
    // tasks own association, the UI loop polls the lease for the IP.
    let rng = Rng::new();
    let [a0, a1, a2, a3] = rng.random().to_be_bytes();
    let [b0, b1, b2, b3] = rng.random().to_be_bytes();
    let seed = u64::from_be_bytes([a0, a1, a2, a3, b0, b1, b2, b3]);
    let stack = enc_net::start(&spawner, peripherals.WIFI, seed, &APP_STATE);
    if stack.is_none() {
        log::error!("net: Wi-Fi stack unavailable");
    }
    log::info!(
        "heap: internal free={} used={} | psram free={} used={}",
        esp_alloc::HEAP.free(),
        esp_alloc::HEAP.used(),
        heap::PSRAM_HEAP.free(),
        heap::PSRAM_HEAP.used(),
    );

    // Buzzer/haptic task (also emits the boot beep).
    match buzzer::task(peripherals.LEDC, peripherals.GPIO17) {
        Ok(token) => spawner.spawn(token),
        Err(_) => log::error!("boot: failed to spawn buzzer task"),
    }

    // Entropy source for the BLE security manager. It is an RAII guard: the
    // TRNG is only available while this is alive, and `main` never returns, so
    // binding it here keeps it up for the life of the program.
    let _trng_source = esp_hal::rng::TrngSource::new(peripherals.RNG, peripherals.ADC1);

    // BLE HID keyboard for the macropad app.
    match ble::task(peripherals.BT, &APP_STATE) {
        Ok(token) => spawner.spawn(token),
        Err(_) => log::error!("boot: failed to spawn ble task"),
    }

    // Bring up the CO5300 display.
    let mut delay = esp_hal::delay::Delay::new();
    let pins = display::DisplayPins {
        en: peripherals.GPIO3,
        rst: peripherals.GPIO4,
        cs: peripherals.GPIO10,
        sclk: peripherals.GPIO12,
        sio0: peripherals.GPIO11,
        sio1: peripherals.GPIO13,
        sio2: peripherals.GPIO7,
        sio3: peripherals.GPIO14,
    };
    let mut panel = match display::init(peripherals.SPI2, peripherals.DMA_CH0, pins, &mut delay) {
        Ok(panel) => Some(panel),
        Err(display::DisplayInitError::DmaBuffer) => {
            log::error!("display: DMA buffer setup failed");
            None
        }
        Err(display::DisplayInitError::Spi(e)) => {
            log::error!("display: SPI config error: {e:?}");
            None
        }
        Err(display::DisplayInitError::Controller(e)) => {
            log::error!("display: controller init error: {e:?}");
            None
        }
    };

    // Encoder (PCNT) + button.
    let encoder_hw = input::EncoderHw::new(peripherals.PCNT, peripherals.GPIO1, peripherals.GPIO2);
    let button = Input::new(
        peripherals.GPIO0,
        InputConfig::default().with_pull(Pull::Up),
    );
    let mut encoder = Encoder::new(COUNTS_PER_DETENT);

    // CHSC5816 touch. The panel belongs to the router, which recognises swipes
    // and taps and turns them into navigation; apps see no samples unless their
    // manifest asks for the raw panel. Touch is optional — the encoder drives
    // everything on its own — so a failure here is logged and life goes on.
    match touch::init(touch::TouchPins {
        i2c: peripherals.I2C0,
        sda: peripherals.GPIO5,
        scl: peripherals.GPIO6,
        int: peripherals.GPIO9,
        rst: peripherals.GPIO8,
    })
    .await
    {
        Some(device) => match touch::task(device) {
            Ok(token) => spawner.spawn(token),
            Err(_) => log::error!("boot: failed to spawn touch task"),
        },
        None => log::error!("boot: touch unavailable"),
    }

    // The framebuffer lives at the base of PSRAM; only build it if PSRAM is
    // actually mapped and large enough (else `from_raw_parts_mut` is UB).
    let framebuffer = heap::framebuffer(psram_start, psram_size, psram_ok, DISPLAY_BYTES);
    if let (Some(panel), Some(fb_buf)) = (panel.as_mut(), framebuffer) {
        // The framebuffer goes into `Ui` and is never seen again: nothing out
        // here writes pixels, so nothing out here needs the bytes.
        let mut slint_ui = match ui::Ui::new(fb_buf) {
            Ok(slint_ui) => slint_ui,
            Err(e) => {
                log::error!("ui: Slint init failed: {e}");
                loop {
                    Timer::after(Duration::from_secs(5)).await;
                }
            }
        };

        // The app registry. Adding an app is its constructor plus one line
        // here — no enum variant, no match arm. `main` never returns, so these
        // locals live for the whole program and need no `StaticCell`.
        let pomodoro = apps::PomodoroFactory::new(slint_ui.shell().as_weak());
        let macropad = apps::MacropadFactory::new(slint_ui.shell().as_weak());
        let registry: [&dyn AppFactory; 2] = [&pomodoro, &macropad];
        let mut router = Router::new(&registry, launcher::default_carousel(0));

        slint_ui.shell().set_cards(app_cards(router.factories()));
        slint_ui.shell().set_selected(0);

        // Initial paint: the launcher, since that is where the router starts.
        let ctx = Ctx {
            now_ms: now_ms(),
            state: &APP_STATE,
        };
        ui::set_now_ms(ctx.now_ms);
        // Render and flush are one call now, so this is the pair's total. The
        // split (~35 ms render, ~21 ms flush for a full frame) needs an
        // instrumented build to recover, which is what it took to measure
        // anyway.
        let started = Instant::now();
        let first = slint_ui.render(panel);
        let frame_us = started.elapsed().as_micros();
        match first {
            Ok(_) => log::info!("slint: first frame {frame_us}us"),
            Err(e) => log::error!("slint: first frame failed: {e}"),
        }
        // Repaints since boot — only frames Slint actually drew, not loop
        // iterations, which is the number worth knowing.
        let mut frames: u32 = 0;

        let mut press_start: Option<Instant> = None;
        // Whether the current hold already fired its long press.
        let mut long_fired = false;
        let mut had_ip = false;
        // Absolute Unix minute last observed / last fired, so the alarm fires
        // exactly once per minute slot on a real edge (never on the first
        // synced sample, and robust to SNTP wall-clock steps).
        let mut last_minute_slot: Option<u32> = None;
        let mut fired_slot: Option<u32> = None;
        let mut persister = settings::Persister::new(saved);

        // Terminal boot marker: everything is up and the UI loop is about to
        // start. `just flash-log` watches for this line and exits, so keep the
        // wording stable — and short, since a line over the 64-byte
        // USB-Serial/JTAG FIFO blocks until the host drains it.
        log::info!("boot: ready");

        loop {
            // Fixed 5ms tick keeps the encoder/button responsive; touch samples
            // arrive asynchronously from the touch task via `touch::SAMPLES`.
            Timer::after(Duration::from_millis(5)).await;
            let mut changed = false;
            let ctx = Ctx {
                now_ms: now_ms(),
                state: &APP_STATE,
            };

            // Encoder → router (carousel, or the active app).
            let detents = encoder.update(encoder_hw.raw());
            if detents != 0 {
                changed |= router.handle(UiInput::Rotate(detents), &ctx);
                buzzer::signal(Feedback::Beep);
            }

            // Button: the router decides what a press means — long-press is
            // "back to launcher", short-press launches or is the app's Select.
            //
            // Long-press fires **the moment the threshold is crossed**, while
            // the button is still down, and buzzes to say so. Waiting for
            // release gave no feedback about when you had held it long enough.
            // The short press then fires on release, but only if the long press
            // did not already claim this hold.
            let down = button.is_low(); // active-low (pull-up + button to GND)
            // The gesture recogniser needs the *contact*, not the press event:
            // pressing the encoder also registers a touch, and that phantom has
            // to be discarded before it navigates anywhere.
            router.set_button(down, ctx.now_ms);
            if down {
                match press_start {
                    None => press_start = Some(Instant::now()),
                    Some(start)
                        if !long_fired && Instant::now().duration_since(start) >= LONG_PRESS =>
                    {
                        long_fired = true;
                        changed |= router.handle(UiInput::LongPress, &ctx);
                        buzzer::signal(Feedback::Haptic);
                    }
                    Some(_) => {}
                }
            } else {
                if press_start.take().is_some() && !long_fired {
                    changed |= router.handle(UiInput::ShortPress, &ctx);
                    buzzer::signal(Feedback::Beep);
                }
                long_fired = false;
            }

            // Touch → router. Drained to empty so a stroke's `Up` is never left
            // queued behind a slow frame, which would strand the gesture.
            while let Ok(sample) = touch::SAMPLES.try_receive() {
                changed |= router.handle(UiInput::Touch(sample), &ctx);
            }

            // Observe the DHCP lease: publish `Connected` only with an IPv4
            // (the connection task owns `Connecting`/`Disconnected`). Log the
            // address once when it first appears.
            if let Some(stack) = stack {
                if let Some(cfg) = stack.config_v4() {
                    let octets = cfg.address.address().octets();
                    APP_STATE.set_ip(octets);
                    APP_STATE.set_conn(ConnState::Connected);
                    if !had_ip {
                        had_ip = true;
                        let [a, b, c, d] = octets;
                        log::info!("net: ip={a}.{b}.{c}.{d}");
                    }
                } else {
                    APP_STATE.clear_ip();
                    had_ip = false;
                }
            }

            // Alarm: beep once when the hour hand reaches the armed mark. Keyed
            // on the absolute Unix minute so a real minute edge (not the first
            // synced sample or an SNTP step) triggers exactly one fire per slot.
            // Fires on any screen since the alarm lives in shared state.
            if let Some(epoch) = APP_STATE.current_epoch(uptime_secs()) {
                let slot = epoch.checked_div(60).unwrap_or(0);
                if last_minute_slot != Some(slot) {
                    if last_minute_slot.is_some() {
                        let minute12 = u16::try_from(slot.rem_euclid(720)).unwrap_or(0);
                        if APP_STATE.alarm() == Some(minute12) && fired_slot != Some(slot) {
                            fired_slot = Some(slot);
                            log::info!("alarm: fired (12h-minute {minute12})");
                            buzzer::signal(Feedback::Haptic);
                        }
                    }
                    last_minute_slot = Some(slot);
                }
            }

            // Persist alarm/toggles to flash, debounced so a burst of edits
            // becomes one write a couple of seconds after the change settles.
            persister.poll(&APP_STATE);

            // Animate / adopt external state, then render + flush the dirty
            // area. `ctx` is re-read here so a carousel slide is sampled at the
            // moment it is drawn rather than at the top of the tick.
            let ctx = Ctx {
                now_ms: now_ms(),
                state: &APP_STATE,
            };
            changed |= router.tick(&ctx);

            // Everything is Slint now: publish state, then let it decide what
            // actually changed. `draw_if_needed` is cheap when nothing did, so
            // this runs unconditionally rather than being gated on `dirty`.
            ui::set_now_ms(ctx.now_ms);
            // The host publishes the active view id and never learns which app
            // it belongs to — that is the whole point of `Manifest::view`.
            slint_ui.shell().set_view(i32::from(router.view_id().0));
            match router.view() {
                View::Launcher => slint_ui
                    .shell()
                    .set_selected(i32::try_from(router.selected()).unwrap_or(0)),
                // Only republish when the app says something changed: setting a
                // struct property unconditionally would dirty Slint every tick
                // and repaint at full loop speed.
                View::App(_) => {
                    if changed {
                        router.sync_app();
                    }
                }
            }

            // Apps cannot reach the radio either; a chord becomes a press and
            // release report, dropped if the queue is full rather than blocking
            // the UI loop for a host that may not even be paired.
            if let Some(chord) = router.take_keys() {
                ble::send_chord(chord.modifiers, chord.usage);
            }

            // Apps cannot reach the buzzer; the router collects their requests.
            if let Some(feedback) = router.take_feedback() {
                buzzer::signal(match feedback {
                    launcher::Feedback::Beep => Feedback::Beep,
                    launcher::Feedback::Haptic => Feedback::Haptic,
                });
            }

            let started = Instant::now();
            match slint_ui.render(panel) {
                // Nothing changed — the overwhelmingly common case.
                Ok(None) => {}
                Ok(Some(rect)) => {
                    frames = frames.saturating_add(1);
                    if frames <= FRAME_LOG_FIRST || frames.checked_rem(FRAME_LOG_EVERY) == Some(0) {
                        let us = started.elapsed().as_micros();
                        let (w, h, x, y) = (rect.w, rect.h, rect.x, rect.y);
                        log::info!("slint: frame {frames} {w}x{h}+{x},{y} {us}us");
                    }
                }
                Err(e) => log::error!("display: {e}"),
            }
        }
    }

    log::info!("enc-app: display unavailable, idling");
    loop {
        Timer::after(Duration::from_secs(5)).await;
    }
}
