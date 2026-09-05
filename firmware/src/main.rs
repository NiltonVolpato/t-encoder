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
mod touch;

use buzzer::Feedback;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer};
use enc_co5300::FrameBuffer;
use enc_input::Encoder;
use enc_state::{AppState, ConnState};
use enc_touch::TouchPoint;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::psram;
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
// `Input` is aliased to `UiInput`: esp-hal's GPIO `Input` already owns that name.
use launcher::{App, Ctx, Dirty, Input as UiInput, Router, View, render_launcher};

esp_bootloader_esp_idf::esp_app_desc!();

/// Tap-down events from the touch task (queued so none are lost during a redraw).
static TOUCH_TAPS: Channel<CriticalSectionRawMutex, TouchPoint, 4> = Channel::new();

/// Panel dimensions (mirror `enc_config::display`).
const DISPLAY_W: u16 = 390;
const DISPLAY_H: u16 = 390;
const DISPLAY_BYTES: usize = enc_config::display::FRAMEBUFFER_BYTES;
/// Quadrature counts per mechanical detent (this encoder emits 2 per click).
const COUNTS_PER_DETENT: u8 = 2;
/// PSRAM smoke-test probe length (top of PSRAM); also reserved from the heap.
const PSRAM_PROBE_LEN: usize = 4096;

/// Shared, lock-free app state mirrored between the UI loop and the Wi-Fi tasks.
static APP_STATE: AppState = AppState::new(apps::MENU_ITEMS.len());

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

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    esp_println::println!("PANIC: {info}");
    loop {
        core::hint::spin_loop();
    }
}

/// Touch task: the CHSC5816's INT pulses are unreliable but its point register
/// stays live while a finger is down, so poll it and queue one tap per press
/// (on the touch-down edge).
#[embassy_executor::task]
async fn touch_task(mut touch: touch::Touch) {
    let mut was_touched = false;
    loop {
        match touch.read_point().await {
            Ok(Some(point)) => {
                if !was_touched {
                    was_touched = true;
                    let _ = TOUCH_TAPS.try_send(point); // drop if the queue is full
                }
            }
            Ok(None) => was_touched = false,
            Err(_) => log::error!("touch: read failed"),
        }
        Timer::after(Duration::from_millis(20)).await;
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

    // CHSC5816 touch task.
    let touch = touch::init(
        touch::TouchPins {
            i2c: peripherals.I2C0,
            sda: peripherals.GPIO5,
            scl: peripherals.GPIO6,
            int: peripherals.GPIO9,
            rst: peripherals.GPIO8,
        },
        enc_config::i2c::CHSC5816_ADDRESS,
    )
    .await;
    if let Some(t) = touch {
        match touch_task(t) {
            Ok(token) => spawner.spawn(token),
            Err(_) => log::error!("boot: failed to spawn touch task"),
        }
    }

    // The app registry. Adding an app is its constructor plus one line here —
    // no enum variant, no match arm. `main` never returns, so these locals
    // live for the whole program and need no `StaticCell`.
    let mut menu = apps::menu_app();
    let mut clock = apps::clock_app();
    let mut registry: [&mut dyn App; 2] = [&mut menu, &mut clock];
    let mut router = Router::new(&mut registry, launcher::default_carousel(0));

    // The framebuffer lives at the base of PSRAM; only build it if PSRAM is
    // actually mapped and large enough (else `from_raw_parts_mut` is UB).
    let framebuffer = psram_framebuffer(psram_start, psram_size, psram_ok);
    if let (Some(panel), Some(fb_buf)) = (panel.as_mut(), framebuffer) {
        // P2 Slint spike: paint one Slint frame straight into the PSRAM buffer
        // before it is wrapped as a `FrameBuffer`, timing the render so the
        // frame-time criterion has a real number. The frame stays on screen
        // until the first repaint, which is convenient for eyeballing quality.
        #[cfg(feature = "slint-spike")]
        match spike_slint::init() {
            Ok((window, _ui)) => {
                spike_slint::set_now_ms(now_ms());
                let started = Instant::now();
                let drawn = spike_slint::render_frame(&window, fb_buf);
                let render_us = started.elapsed().as_micros();

                let started = Instant::now();
                let flush_ok = panel.driver.flush(fb_buf, display::DMA_CHUNK).is_ok();
                let flush_us = started.elapsed().as_micros();

                log::info!(
                    "slint: drawn={drawn} render={render_us}us flush={flush_us}us ok={flush_ok}"
                );
                log::info!(
                    "slint: heap internal free={} used={}",
                    esp_alloc::HEAP.free(),
                    esp_alloc::HEAP.used(),
                );
            }
            Err(e) => log::error!("slint: init failed: {e}"),
        }

        let mut fb = FrameBuffer::new(fb_buf, DISPLAY_W, DISPLAY_H);

        // Initial paint: the launcher, since that is where the router starts.
        let ctx = Ctx {
            now_ms: now_ms(),
            state: &APP_STATE,
        };
        // Timed once at boot so the embedded-graphics baseline has the same
        // numbers the Slint spike reports, rather than an estimate.
        let started = Instant::now();
        render_launcher(&router, &ctx, &mut fb);
        let render_us = started.elapsed().as_micros();
        let started = Instant::now();
        let flush_ok = panel.driver.flush(fb.bytes(), display::DMA_CHUNK).is_ok();
        let flush_us = started.elapsed().as_micros();
        log::info!("baseline: render={render_us}us flush={flush_us}us ok={flush_ok}");
        if !flush_ok {
            log::error!("display: initial flush failed");
        }

        let mut press_start: Option<Instant> = None;
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
            // The action fires on release so its duration is known.
            let down = button.is_low(); // active-low (pull-up + button to GND)
            if down && press_start.is_none() {
                press_start = Some(Instant::now());
            } else if !down && let Some(start) = press_start.take() {
                let input = if Instant::now().duration_since(start) >= Duration::from_millis(600) {
                    UiInput::LongPress
                } else {
                    UiInput::ShortPress
                };
                dirty = dirty.merge(router.handle(input, &ctx));
                buzzer::signal(Feedback::Beep);
            }

            // Touch taps → router (none lost during a redraw).
            while let Ok(point) = TOUCH_TAPS.try_receive() {
                let input = UiInput::Touch {
                    x: i32::from(point.x),
                    y: i32::from(point.y),
                };
                dirty = dirty.merge(router.handle(input, &ctx));
                buzzer::signal(Feedback::Beep);
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
            if dirty != Dirty::None {
                match router.view() {
                    View::Launcher => render_launcher(&router, &ctx, &mut fb),
                    View::App(_) => router.render_app(&ctx, &mut fb),
                }
                let flushed = match dirty {
                    Dirty::Full => match panel.driver.flush(fb.bytes(), display::DMA_CHUNK) {
                        Ok(()) => true,
                        Err(e) => {
                            log::error!("display: full flush failed: {e:?}");
                            false
                        }
                    },
                    Dirty::Band { y, h } => flush_band(panel, fb.bytes(), y, h),
                    Dirty::None => true,
                };
                // Repair the whole frame if a band flush failed.
                if !flushed {
                    log::warn!("display: repairing frame after a failed flush");
                    if let Err(e) = panel.driver.flush(fb.bytes(), display::DMA_CHUNK) {
                        log::error!("display: repair flush also failed: {e:?}");
                    }
                }
            }
        }
    }

    log::info!("enc-app: display unavailable, idling");
    loop {
        Timer::after(Duration::from_secs(5)).await;
    }
}

/// Builds the PSRAM-backed framebuffer slice, or `None` if PSRAM is unavailable
/// or smaller than a full frame. Gating here keeps `from_raw_parts_mut` from
/// ever running on an invalid (e.g. `0..0`) range.
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
