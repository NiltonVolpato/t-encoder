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

#[cfg(feature = "radio")]
mod ble;
mod buzzer;
mod display;
mod event;
mod heap;
mod input;
mod logger;
mod radio;
mod serial;
mod touch;

use buzzer::Feedback;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::psram;
use esp_hal::timer::timg::TimerGroup;
use event::{EVENTS, Event};
use launcher::{AppFactory, Ctx, Router, View};
use slint::ComponentHandle;

esp_bootloader_esp_idf::esp_app_desc!();

/// Framebuffer size (mirrors `enc_config::display`); the panel's own width and
/// height live in `display::Display`, which does the clipping.
const DISPLAY_BYTES: usize = enc_config::display::FRAMEBUFFER_BYTES;
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
    // `checked_div` rather than a guarded `/`: every caller passes a non-zero
    // max, and this keeps that fact checked rather than assumed.
    let scale = |value: u8, max: u8| -> u8 {
        let widened = u16::from(value)
            .saturating_mul(255)
            .checked_div(u16::from(max))
            .unwrap_or(0);
        u8::try_from(widened).unwrap_or(0)
    };
    slint::Color::from_rgb_u8(
        scale(color.r(), 31),
        scale(color.g(), 63),
        scale(color.b(), 31),
    )
}

/// Prints the panic and stops. Reachable only while the release profile keeps
/// `panic = "abort"`: switching to `immediate-abort` compiles `panic!` straight
/// to an abort and deletes this message from the binary entirely, which is the
/// trade documented in the root `Cargo.toml`.
///
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    esp_println::println!("PANIC: {info}");
    firmware_panic_stop()
}

/// Where a panic comes to rest, split out only to carry a fixed symbol name, so
/// a debugger has somewhere to stop: under QEMU, `hbreak firmware_panic_stop`
/// catches every panic without needing an address. Three things that do not
/// work in its place — `#[panic_handler]` ignores `export_name`, Rust's own
/// `rust_begin_unwind` carries a per-build hash and so cannot be named, and a
/// `break` instruction never reaches the gdb stub because esp-hal's exception
/// handler catches the debug exception itself and panics "Breakpoint on
/// `ProCpu`".
/// `inline(never)` is what makes the symbol survive LTO. On device this is only
/// a name in a symbol table the release profile then strips.
#[unsafe(no_mangle)]
#[inline(never)]
extern "C" fn firmware_panic_stop() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

/// Encapsulates device hardware, UI renderer, and app router.
pub struct Device {
    panel: display::Display,
    slint_ui: ui::Ui,
    router: Router<'static>,
    _radio: radio::Guard,
    frames: u32,
}

impl Device {
    /// Initializes all board peripherals, memory heaps, tasks, and UI state.
    #[expect(clippy::too_many_lines)]
    pub async fn setup(spawner: Spawner) -> Self {
        let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
        let peripherals = esp_hal::init(config);

        logger::init();

        // Internal-only global heap (esp_alloc::HEAP) — serves esp-radio + DMA.
        esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: heap::INTERNAL_HEAP_RECLAIMED);
        esp_alloc::heap_allocator!(size: heap::INTERNAL_HEAP_EXTRA);

        let timg0 = TimerGroup::new(peripherals.TIMG0);
        let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
        esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

        log::info!(
            "t-encoder firmware: built by {} @ {} on {}",
            env!("BUILD_USER"),
            env!("BUILD_HOST"),
            env!("BUILD_DATE")
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
            if !heap::init_psram_heap(psram_start, psram_size, DISPLAY_BYTES, heap::PROBE_LEN) {
                log::error!("psram: heap region not registered (range invalid/too small)");
            }
        } else {
            log::error!("psram: smoke test FAILED — check PSRAM mode (octal vs quad)");
        }

        // Wi-Fi STA + embassy-net and BLE HID keyboard.
        let (_stack, radio_guard) = radio::start(
            spawner,
            radio::Parts {
                wifi: peripherals.WIFI,
                bt: peripherals.BT,
                rng: peripherals.RNG,
                adc1: peripherals.ADC1,
            },
        );
        log::info!(
            "heap: internal free={} used={} | psram free={} used={}",
            esp_alloc::HEAP.free(),
            esp_alloc::HEAP.used(),
            heap::PSRAM_HEAP.free(),
            heap::PSRAM_HEAP.used(),
        );

        // Buzzer/haptic task (emits the boot beep).
        match buzzer::task(peripherals.LEDC, peripherals.GPIO17) {
            Ok(token) => spawner.spawn(token),
            Err(_) => log::error!("boot: failed to spawn buzzer task"),
        }

        // Serial NDJSON tasks over native USB-Serial/JTAG.
        let usb_serial =
            esp_hal::usb_serial_jtag::UsbSerialJtag::new(peripherals.USB_DEVICE).into_async();
        let (rx, tx) = usb_serial.split();
        match serial::tx_task(tx) {
            Ok(token) => spawner.spawn(token),
            Err(_) => log::error!("boot: failed to spawn serial tx task"),
        }
        match serial::rx_task(rx) {
            Ok(token) => spawner.spawn(token),
            Err(_) => log::error!("boot: failed to spawn serial rx task"),
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
        let panel = match display::init(peripherals.SPI2, peripherals.DMA_CH0, pins, &mut delay) {
            Ok(panel) => panel,
            Err(display::DisplayInitError::DmaBuffer) => {
                log::error!("display: DMA buffer setup failed");
                loop {
                    Timer::after(Duration::from_secs(5)).await;
                }
            }
            Err(display::DisplayInitError::Spi(e)) => {
                log::error!("display: SPI config error: {e:?}");
                loop {
                    Timer::after(Duration::from_secs(5)).await;
                }
            }
            Err(display::DisplayInitError::Controller(e)) => {
                log::error!("display: controller init error: {e:?}");
                loop {
                    Timer::after(Duration::from_secs(5)).await;
                }
            }
        };

        // Encoder (PCNT) edge interrupt task.
        let encoder_hw =
            input::EncoderHw::new(peripherals.PCNT, peripherals.GPIO1, peripherals.GPIO2);
        match input::encoder_task(encoder_hw) {
            Ok(token) => spawner.spawn(token),
            Err(_) => log::error!("boot: failed to spawn encoder task"),
        }

        // Button edge interrupt task.
        let button = Input::new(
            peripherals.GPIO0,
            InputConfig::default().with_pull(Pull::Up),
        );
        match input::button_task(button) {
            Ok(token) => spawner.spawn(token),
            Err(_) => log::error!("boot: failed to spawn button task"),
        }

        // CHSC5816 touch task.
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

        // PSRAM Framebuffer.
        let framebuffer = heap::framebuffer(psram_start, psram_size, psram_ok, DISPLAY_BYTES);
        let Some(fb_buf) = framebuffer else {
            log::error!("boot: framebuffer unavailable, idling");
            loop {
                Timer::after(Duration::from_secs(5)).await;
            }
        };

        // Slint UI.
        let slint_ui = match ui::Ui::new(fb_buf) {
            Ok(slint_ui) => slint_ui,
            Err(e) => {
                log::error!("ui: Slint init failed: {e}");
                loop {
                    Timer::after(Duration::from_secs(5)).await;
                }
            }
        };

        // App registry & Router.
        let pomodoro: &'static dyn AppFactory = alloc::boxed::Box::leak(alloc::boxed::Box::new(
            apps::PomodoroFactory::new(slint_ui.shell().as_weak()),
        ));
        let macropad: &'static dyn AppFactory = alloc::boxed::Box::leak(alloc::boxed::Box::new(
            apps::MacropadFactory::new(slint_ui.shell().as_weak()),
        ));
        let simon: &'static dyn AppFactory = alloc::boxed::Box::leak(alloc::boxed::Box::new(
            apps::SimonFactory::new(slint_ui.shell().as_weak()),
        ));
        let registry: &'static [&'static dyn AppFactory] =
            alloc::boxed::Box::leak(alloc::boxed::Box::new([pomodoro, macropad, simon]));
        let router = Router::new(registry, launcher::default_carousel(0));

        slint_ui.shell().set_cards(app_cards(router.factories()));
        slint_ui.shell().set_selected(0);

        let mut device = Self {
            panel,
            slint_ui,
            router,
            _radio: radio_guard,
            frames: 0,
        };

        // Initial paint: launcher carousel.
        ui::set_now_ms(now_ms());
        let started = Instant::now();
        let first = device.slint_ui.render(&mut device.panel);
        let frame_us = started.elapsed().as_micros();
        match first {
            Ok(_) => log::info!("slint: first frame {frame_us}us"),
            Err(e) => log::error!("slint: first frame failed: {e}"),
        }

        // Terminal boot marker and event.
        log::info!("boot: ready");
        serial::broadcast_event(protocol::DeviceEvent::Boot { ready: true });

        device
    }

    /// Runs the event-driven main loop.
    pub async fn run(&mut self) -> ! {
        let mut drew_frame = false;
        loop {
            let timeout = if self.slint_ui.has_active_animations() || drew_frame {
                Duration::from_millis(16)
            } else {
                Duration::from_secs(1)
            };

            let event = match select(EVENTS.receive(), Timer::after(timeout)).await {
                Either::First(ev) => Some(ev),
                Either::Second(()) => None,
            };

            let Some(event) = event else {
                drew_frame = self.render(false);
                continue;
            };

            let ctx = Ctx {
                now_ms: now_ms(),
                ble_linked: radio::ble_linked(),
            };

            log::info!("event @ {}ms: {:?}", ctx.now_ms, event);

            let changed = match event {
                Event::Rotate(detents) => {
                    buzzer::signal(Feedback::Beep);
                    self.router.handle(launcher::Input::Rotate(detents), &ctx)
                }
                Event::ShortPress => {
                    buzzer::signal(Feedback::Beep);
                    self.router.handle(launcher::Input::ShortPress, &ctx)
                }
                Event::LongPress => {
                    buzzer::signal(Feedback::Haptic);
                    self.router.handle(launcher::Input::LongPress, &ctx)
                }
                Event::Gesture(gesture) => self.router.handle_gesture(gesture, &ctx),
            };

            drew_frame = self.render(changed);
        }
    }

    /// Animates, syncs app state, handles keys/buzzer feedback, and repaints dirty regions.
    /// Returns true if a frame was rendered and flushed to the panel.
    fn render(&mut self, event_changed: bool) -> bool {
        let ctx = Ctx {
            now_ms: now_ms(),
            ble_linked: radio::ble_linked(),
        };

        let tick_changed = self.router.tick(&ctx);
        let changed = event_changed || tick_changed;

        ui::set_now_ms(ctx.now_ms);
        self.slint_ui
            .shell()
            .set_view(i32::from(self.router.view_id().0));
        match self.router.view() {
            View::Launcher => self
                .slint_ui
                .shell()
                .set_selected(i32::try_from(self.router.selected()).unwrap_or(0)),
            View::App(_) => {
                if changed {
                    self.router.sync_app();
                }
            }
        }

        if changed {
            let (screen, card, app) = match self.router.view() {
                View::Launcher => (
                    alloc::string::ToString::to_string("launcher"),
                    Some(self.router.selected()),
                    None,
                ),
                View::App(idx) => (
                    alloc::string::ToString::to_string("app"),
                    None,
                    self.router
                        .factories()
                        .get(idx)
                        .map(|f| alloc::string::ToString::to_string(f.manifest().name)),
                ),
            };
            serial::broadcast_event(protocol::DeviceEvent::ViewChanged { screen, card, app });
        }

        if let Some(chord) = self.router.take_keys() {
            radio::send_chord(chord.modifiers, chord.usage);
        }

        if let Some(feedback) = self.router.take_feedback() {
            buzzer::signal(feedback);
        }

        let started = Instant::now();
        match self.slint_ui.render(&mut self.panel) {
            Ok(None) => false,
            Ok(Some(rect)) => {
                self.frames = self.frames.saturating_add(1);
                let us = started.elapsed().as_micros();
                let anim = self.slint_ui.has_active_animations();
                let (w, h, x, y) = (rect.w, rect.h, rect.x, rect.y);
                if self.frames <= 5 || self.frames.checked_rem(500) == Some(0) {
                    log::info!(
                        "slint: frame {} @ {}ms ({us}us) anim={anim} {w}x{h}+{x},{y}",
                        self.frames,
                        ctx.now_ms
                    );
                }
                true
            }
            Err(e) => {
                log::error!("display: {e}");
                false
            }
        }
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let mut device = Device::setup(spawner).await;
    device.run().await;
}
