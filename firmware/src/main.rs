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

mod buzzer;
mod display;
mod heap;
mod input;
mod settings;

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
use launcher::{AppFactory, Ctx, Dirty, Input as UiInput, Router, View};
// `as_weak` on the generated Slint component comes from this trait.
use slint::ComponentHandle;

esp_bootloader_esp_idf::esp_app_desc!();

/// Panel dimensions (mirror `enc_config::display`).
const DISPLAY_W: u16 = 390;
const DISPLAY_BYTES: usize = enc_config::display::FRAMEBUFFER_BYTES;
/// How long the button must be held before the long press fires.
const LONG_PRESS: Duration = Duration::from_millis(600);
/// Quadrature counts per mechanical detent (this encoder emits 2 per click).
const COUNTS_PER_DETENT: u8 = 2;
/// PSRAM smoke-test probe length (top of PSRAM); also reserved from the heap.
const PSRAM_PROBE_LEN: usize = 4096;

/// Shared, lock-free app state mirrored between the UI loop and the Wi-Fi tasks.
static APP_STATE: AppState = AppState::new(1);

/// Flushes a full-width horizontal band `[y, y+h)` of the framebuffer to the
/// panel. Returns whether the band was flushed (false if out of range or DMA
/// failed, so the caller can fall back to a full-frame flush).
fn flush_band(display: &mut display::Display, fb_bytes: &[u8], y: u16, h: u16) -> bool {
    let row_bytes = usize::from(DISPLAY_W).saturating_mul(2);
    let start = usize::from(y).saturating_mul(row_bytes);
    let end = start.saturating_add(usize::from(h).saturating_mul(row_bytes));
    // Both failure modes are logged separately: a silent `false` here used to
    // be indistinguishable from a DMA error, which made display corruption
    // impossible to diagnose from a serial log.
    let Some(band) = fb_bytes.get(start..end) else {
        log::error!("display: band y={y} h={h} out of framebuffer range");
        return false;
    };
    match display
        .driver
        .flush_window(0, y, DISPLAY_W, h, band, display::DMA_CHUNK)
    {
        Ok(()) => true,
        Err(e) => {
            log::error!("display: band flush y={y} h={h} failed: {e:?}");
            false
        }
    }
}

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
    let psram_ok = psram_smoke_test(psram_start, psram_size);
    if psram_ok {
        log::info!("psram: smoke test OK (octal mode confirmed)");
        // Separate (non-global) PSRAM heap for app bulk, past the framebuffer.
        if !heap::init_psram_heap(psram_start, psram_size, DISPLAY_BYTES, PSRAM_PROBE_LEN) {
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

    // Touch is DISABLED until gestures land, and its driver has been removed
    // rather than left dead: raw taps reaching apps made them unusable, because
    // pressing the encoder also registers a touch, so every press delivered a
    // spurious tap on top of it. Nothing polls the I2C bus now. Gesture
    // recognition needs swipe tracking rather than the old tap-per-press
    // model, so it lands as new code — see the touch section of the plan.

    // The framebuffer lives at the base of PSRAM; only build it if PSRAM is
    // actually mapped and large enough (else `from_raw_parts_mut` is UB).
    let framebuffer = psram_framebuffer(psram_start, psram_size, psram_ok);
    if let (Some(panel), Some(fb_buf)) = (panel.as_mut(), framebuffer) {
        // Slint owns every pixel now, so the framebuffer stays a plain byte
        // slice that only `ui` writes to.
        let slint_ui = match ui::Ui::new() {
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
        let registry: [&dyn AppFactory; 1] = [&pomodoro];
        let mut router = Router::new(&registry, launcher::default_carousel(0));

        slint_ui.shell().set_cards(app_cards(router.factories()));
        slint_ui.shell().set_selected(0);

        // Initial paint: the launcher, since that is where the router starts.
        let ctx = Ctx {
            now_ms: now_ms(),
            state: &APP_STATE,
        };
        ui::set_now_ms(ctx.now_ms);
        let started = Instant::now();
        let rect = slint_ui.render(fb_buf);
        let render_us = started.elapsed().as_micros();
        let started = Instant::now();
        let flushed = rect.is_some_and(|r| flush_band(panel, fb_buf, r.y, r.h));
        let flush_us = started.elapsed().as_micros();
        log::info!(
            "slint: first frame {rect:?} render={render_us}us flush={flush_us}us ok={flushed}"
        );

        let mut press_start: Option<Instant> = None;
        // Whether the current hold already fired its long press.
        let mut long_fired = false;
        let mut had_ip = false;
        // Absolute Unix minute last observed / last fired, so the alarm fires
        // exactly once per minute slot on a real edge (never on the first
        // synced sample, and robust to SNTP wall-clock steps).
        let mut last_minute_slot: Option<u32> = None;
        let mut fired_slot: Option<u32> = None;
        // Persisted settings + a debounce: save ~2 s after a change settles so
        // rapid toggling collapses to one flash write.
        let mut saved = saved;
        let mut last_pending = saved;
        let mut dirty_since: Option<Instant> = None;
        loop {
            // Fixed 5ms tick keeps the encoder/button responsive; taps arrive
            // asynchronously from the touch task via TOUCH_TAPS.
            Timer::after(Duration::from_millis(5)).await;
            let mut dirty = Dirty::None;
            let ctx = Ctx {
                now_ms: now_ms(),
                state: &APP_STATE,
            };

            // Encoder → router (carousel, or the active app).
            let detents = encoder.update(encoder_hw.raw());
            if detents != 0 {
                dirty = dirty.merge(router.handle(UiInput::Rotate(detents), &ctx));
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
            if down {
                match press_start {
                    None => press_start = Some(Instant::now()),
                    Some(start)
                        if !long_fired && Instant::now().duration_since(start) >= LONG_PRESS =>
                    {
                        long_fired = true;
                        dirty = dirty.merge(router.handle(UiInput::LongPress, &ctx));
                        buzzer::signal(Feedback::Haptic);
                    }
                    Some(_) => {}
                }
            } else {
                if press_start.take().is_some() && !long_fired {
                    dirty = dirty.merge(router.handle(UiInput::ShortPress, &ctx));
                    buzzer::signal(Feedback::Beep);
                }
                long_fired = false;
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
            let pending = settings::Settings {
                alarm: APP_STATE.alarm(),
                toggles: APP_STATE.toggles(),
            };
            if pending == saved {
                dirty_since = None;
                last_pending = saved;
            } else if pending != last_pending {
                // Value still moving — (re)start the settle timer from this change.
                last_pending = pending;
                dirty_since = Some(Instant::now());
            } else if let Some(since) = dirty_since
                && Instant::now().duration_since(since) >= Duration::from_secs(2)
            {
                // Stable for the debounce window — persist once.
                settings::save(&pending);
                saved = pending;
                dirty_since = None;
            }

            // Animate / adopt external state, then render + flush the dirty
            // area. `ctx` is re-read here so a carousel slide is sampled at the
            // moment it is drawn rather than at the top of the tick.
            let ctx = Ctx {
                now_ms: now_ms(),
                state: &APP_STATE,
            };
            dirty = dirty.merge(router.tick(&ctx));

            // Everything is Slint now: publish state, then let it decide what
            // actually changed. `draw_if_needed` is cheap when nothing did, so
            // this runs unconditionally rather than being gated on `dirty`.
            ui::set_now_ms(ctx.now_ms);
            match router.view() {
                View::Launcher => {
                    slint_ui.shell().set_view(ui::ShellView::Launcher);
                    slint_ui
                        .shell()
                        .set_selected(i32::try_from(router.selected()).unwrap_or(0));
                }
                // One app, so one arm. When a second Slint app lands this
                // wants a view id on `Manifest` rather than a match here —
                // otherwise it becomes the per-app match the App trait exists
                // to avoid.
                View::App(_) => {
                    slint_ui.shell().set_view(ui::ShellView::Pomodoro);
                    // Only republish when the app says something changed:
                    // setting a struct property unconditionally would dirty
                    // Slint every tick and repaint at full loop speed.
                    if dirty != Dirty::None {
                        router.sync_app();
                    }
                }
            }

            // Apps cannot reach the buzzer; the router collects their requests.
            if let Some(feedback) = router.take_feedback() {
                buzzer::signal(match feedback {
                    launcher::Feedback::Beep => Feedback::Beep,
                    launcher::Feedback::Haptic => Feedback::Haptic,
                });
            }

            if let Some(rect) = slint_ui.render(fb_buf)
                && !flush_band(panel, fb_buf, rect.y, rect.h)
            {
                log::error!("display: slint flush failed {rect:?}");
            }
        }
    }

    log::info!("enc-app: display unavailable, idling");
    loop {
        Timer::after(Duration::from_secs(5)).await;
    }
}

/// Builds the PSRAM-backed framebuffer, or `None` if PSRAM is unavailable or
/// smaller than a full frame. Gating here keeps `from_raw_parts_mut` from ever
/// running on an invalid (e.g. `0..0`) range.
///
/// One buffer: Slint renders directly in the panel's byte order via a custom
/// `TargetPixel`, so no conversion pass and no second buffer are needed.
fn psram_framebuffer(start: *mut u8, size: usize, ok: bool) -> Option<&'static mut [u8]> {
    if !ok || start.is_null() || size < DISPLAY_BYTES {
        return None;
    }
    // SAFETY: `start`/`size` come from a successful `Psram` init; the region is
    // mapped for the whole program lifetime and is at least `DISPLAY_BYTES`
    // long. The framebuffer sits at the PSRAM base and never overlaps the
    // smoke-test probe (top 4 KiB). `u8` has alignment 1, so the pointer is
    // always suitably aligned.
    Some(unsafe { core::slice::from_raw_parts_mut(start, DISPLAY_BYTES) })
}

/// Writes a 4 KiB pattern to the top of PSRAM, reads it back, and reports
/// whether it round-trips. A failure almost always means the configured PSRAM
/// mode (octal/quad) does not match the module.
fn psram_smoke_test(start: *mut u8, size: usize) -> bool {
    const PROBE_LEN: usize = PSRAM_PROBE_LEN;
    if size < PROBE_LEN {
        log::error!("psram: too small for smoke test ({size} bytes)");
        return false;
    }
    let base = unsafe { start.add(size.saturating_sub(PROBE_LEN)) };
    for i in 0..PROBE_LEN {
        let byte = u8::try_from((i ^ 0xA5) & 0xFF).unwrap_or(0);
        unsafe { core::ptr::write_volatile(base.add(i), byte) };
    }
    for i in 0..PROBE_LEN {
        let want = u8::try_from((i ^ 0xA5) & 0xFF).unwrap_or(0);
        let got = unsafe { core::ptr::read_volatile(base.add(i)) };
        if got != want {
            log::error!("psram: mismatch at {i}: want {want:#04x} got {got:#04x}");
            return false;
        }
    }
    true
}
