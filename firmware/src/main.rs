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
#![feature(impl_trait_in_assoc_type)]

extern crate alloc;

#[cfg(feature = "radio")]
mod ble;
mod buzzer;
pub mod cpu_metrics;
mod display;
mod event;
mod heap;
mod input;
mod logger;
#[cfg(feature = "radio")]
pub mod net;
mod radio;
mod serial;
pub mod storage;
mod touch;

use buzzer::Feedback;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::psram;
use esp_hal::system::Stack;
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

/// Core 1 execution stack (16 KiB, 16-byte aligned).
const APP_CORE_STACK_SIZE: usize = 16 * 1024;
static mut APP_CORE_STACK: Stack<APP_CORE_STACK_SIZE> = Stack::new();

/// Hardware peripherals and framebuffer handed across to Core 1 for HMI and UI.
struct Core1Peripherals {
    spi2: esp_hal::peripherals::SPI2<'static>,
    dma_ch0: esp_hal::peripherals::DMA_CH0<'static>,
    pcnt: esp_hal::peripherals::PCNT<'static>,
    i2c0: esp_hal::peripherals::I2C0<'static>,
    ledc: esp_hal::peripherals::LEDC<'static>,
    gpio0: esp_hal::peripherals::GPIO0<'static>,
    gpio1: esp_hal::peripherals::GPIO1<'static>,
    gpio2: esp_hal::peripherals::GPIO2<'static>,
    gpio3: esp_hal::peripherals::GPIO3<'static>,
    gpio4: esp_hal::peripherals::GPIO4<'static>,
    gpio5: esp_hal::peripherals::GPIO5<'static>,
    gpio6: esp_hal::peripherals::GPIO6<'static>,
    gpio7: esp_hal::peripherals::GPIO7<'static>,
    gpio8: esp_hal::peripherals::GPIO8<'static>,
    gpio9: esp_hal::peripherals::GPIO9<'static>,
    gpio10: esp_hal::peripherals::GPIO10<'static>,
    gpio11: esp_hal::peripherals::GPIO11<'static>,
    gpio12: esp_hal::peripherals::GPIO12<'static>,
    gpio13: esp_hal::peripherals::GPIO13<'static>,
    gpio14: esp_hal::peripherals::GPIO14<'static>,
    gpio17: esp_hal::peripherals::GPIO17<'static>,
    fb_buf: &'static mut [u8],
}

/// Entry point for the Core 1 (`AppCpu`) thread.
fn core1_entry(peripherals: Core1Peripherals) -> ! {
    static CORE1_EXECUTOR: static_cell::StaticCell<esp_rtos::embassy::Executor> =
        static_cell::StaticCell::new();
    let executor = CORE1_EXECUTOR.init(esp_rtos::embassy::Executor::new());
    executor.run_with_callbacks(
        |spawner| match core1_task(spawner, peripherals) {
            Ok(token) => spawner.spawn(token),
            Err(_) => log::error!("core1: failed to spawn core1_task"),
        },
        cpu_metrics::CoreTracker::new(&cpu_metrics::CPU1_USAGE),
    )
}

/// Main async task on Core 1 owning display, touch, encoder, and UI rendering loop.
#[embassy_executor::task]
async fn core1_task(spawner: Spawner, peripherals: Core1Peripherals) {
    let mut device = Device::setup_core1(spawner, peripherals).await;
    device.run().await;
}

/// Encapsulates device hardware, UI renderer, and app router.
pub struct Device {
    panel: display::Display,
    slint_ui: ui::Ui,
    router: Router<'static>,
    frames: u32,
    ble_active: bool,
}

impl Device {
    /// Initializes HMI hardware, display, Slint UI, and router on Core 1.
    #[expect(clippy::too_many_lines)]
    async fn setup_core1(spawner: Spawner, peripherals: Core1Peripherals) -> Self {
        // Buzzer/haptic task (emits the boot beep).
        match buzzer::task(peripherals.ledc, peripherals.gpio17) {
            Ok(token) => spawner.spawn(token),
            Err(_) => log::error!("boot: failed to spawn buzzer task"),
        }

        // Encoder (PCNT) edge interrupt task.
        let encoder_hw =
            input::EncoderHw::new(peripherals.pcnt, peripherals.gpio1, peripherals.gpio2);
        match input::encoder_task(encoder_hw) {
            Ok(token) => spawner.spawn(token),
            Err(_) => log::error!("boot: failed to spawn encoder task"),
        }

        // Button edge interrupt task.
        let button = Input::new(
            peripherals.gpio0,
            InputConfig::default().with_pull(Pull::Up),
        );
        match input::button_task(button) {
            Ok(token) => spawner.spawn(token),
            Err(_) => log::error!("boot: failed to spawn button task"),
        }

        // CHSC5816 touch task.
        match touch::init(touch::TouchPins {
            i2c: peripherals.i2c0,
            sda: peripherals.gpio5,
            scl: peripherals.gpio6,
            int: peripherals.gpio9,
            rst: peripherals.gpio8,
        })
        .await
        {
            Some(device) => match touch::task(device) {
                Ok(token) => spawner.spawn(token),
                Err(_) => log::error!("boot: failed to spawn touch task"),
            },
            None => log::error!("boot: touch unavailable"),
        }

        // Bring up the CO5300 display.
        let mut delay = esp_hal::delay::Delay::new();
        let pins = display::DisplayPins {
            en: peripherals.gpio3,
            rst: peripherals.gpio4,
            cs: peripherals.gpio10,
            sclk: peripherals.gpio12,
            sio0: peripherals.gpio11,
            sio1: peripherals.gpio13,
            sio2: peripherals.gpio7,
            sio3: peripherals.gpio14,
        };
        let panel = match display::init(peripherals.spi2, peripherals.dma_ch0, pins, &mut delay) {
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

        // Slint UI.
        let slint_ui = match ui::Ui::new(peripherals.fb_buf) {
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
            apps::MacropadFactory::with_settings_getter(
                slint_ui.shell().as_weak(),
                crate::storage::get_macropad_settings_sync,
            ),
        ));
        let simon: &'static dyn AppFactory = alloc::boxed::Box::leak(alloc::boxed::Box::new(
            apps::SimonFactory::new(slint_ui.shell().as_weak()),
        ));
        let magic8: &'static dyn AppFactory = alloc::boxed::Box::leak(alloc::boxed::Box::new(
            apps::Magic8Factory::new(slint_ui.shell().as_weak()),
        ));
        let registry: &'static [&'static dyn AppFactory] =
            alloc::boxed::Box::leak(alloc::boxed::Box::new([pomodoro, macropad, simon, magic8]));
        let router = Router::new(registry, launcher::default_carousel(0));

        slint_ui.shell().set_cards(app_cards(router.factories()));
        slint_ui.shell().set_selected(0);

        let mut device = Self {
            panel,
            slint_ui,
            router,
            frames: 0,
            ble_active: false,
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

        let ble_needed = match self.router.view() {
            View::Launcher => false,
            View::App(idx) => self
                .router
                .factories()
                .get(idx)
                .is_some_and(|f| f.manifest().requires_ble),
        };

        if ble_needed != self.ble_active {
            self.ble_active = ble_needed;
            if ble_needed {
                radio::enable_ble();
            } else {
                radio::disable_ble();
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

/// Context and peripherals passed to Core 0 main async task.
struct Core0Parts {
    cpu_ctrl: esp_hal::peripherals::CPU_CTRL<'static>,
    sw_int1: esp_hal::interrupt::software::SoftwareInterrupt<'static, 1>,
    core1_peripherals: Core1Peripherals,
    rx: esp_hal::usb_serial_jtag::UsbSerialJtagRx<'static, esp_hal::Async>,
    tx: esp_hal::usb_serial_jtag::UsbSerialJtagTx<'static, esp_hal::Async>,
    wifi: esp_hal::peripherals::WIFI<'static>,
    bt: esp_hal::peripherals::BT<'static>,
    rng: esp_hal::peripherals::RNG<'static>,
    adc1: esp_hal::peripherals::ADC1<'static>,
}

#[embassy_executor::task]
async fn core0_task(spawner: Spawner, parts: Core0Parts) {
    if let Ok(token) = serial::tx_task(parts.tx) {
        spawner.spawn(token);
    }
    if let Ok(token) = serial::rx_task(parts.rx) {
        spawner.spawn(token);
    }

    // Initialize flash settings storage on Core 0 before starting second core.
    storage::init().await;

    // Start Core 1 (AppCpu) thread.
    esp_rtos::start_second_core::<APP_CORE_STACK_SIZE>(
        parts.cpu_ctrl,
        parts.sw_int1,
        unsafe { &mut *core::ptr::addr_of_mut!(APP_CORE_STACK) },
        move || {
            core1_entry(parts.core1_peripherals);
        },
    );

    // Wi-Fi STA + embassy-net and BLE HID keyboard on Core 0.
    let (_stack, _radio_guard) = radio::start(
        spawner,
        radio::Parts {
            wifi: parts.wifi,
            bt: parts.bt,
            rng: parts.rng,
            adc1: parts.adc1,
        },
    );
    log::info!(
        "heap: internal free={} | psram free={} | total used={}",
        esp_alloc::HEAP.free_caps(esp_alloc::MemoryCapability::Internal.into()),
        esp_alloc::HEAP.free_caps(esp_alloc::MemoryCapability::External.into()),
        esp_alloc::HEAP.used(),
    );

    // Core 0 loop: ticks every 1s to keep _radio_guard alive and prevent cycle overflow.
    loop {
        Timer::after(Duration::from_secs(1)).await;
    }
}

static CORE0_EXECUTOR: static_cell::StaticCell<esp_rtos::embassy::Executor> =
    static_cell::StaticCell::new();

#[esp_hal::main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    logger::init();

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
    let psram_ok = heap::smoke_test(psram_start, psram_size);
    if psram_ok {
        if !heap::init_psram_heap(psram_start, psram_size, DISPLAY_BYTES, heap::PROBE_LEN) {
            log::error!("psram: heap region not registered (range invalid/too small)");
        }
    } else {
        log::error!("psram: smoke test FAILED — check PSRAM mode (octal vs quad)");
    }

    // Register internal heap regions after PSRAM so PSRAM is Region 0 (default for general alloc).
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
    log::info!("psram: {} KiB mapped at {psram_start:p}", psram_size / 1024);
    if psram_ok {
        log::info!("psram: smoke test OK (octal mode confirmed)");
    }

    // PSRAM Framebuffer.
    let framebuffer = heap::framebuffer(psram_start, psram_size, psram_ok, DISPLAY_BYTES);
    let Some(fb_buf) = framebuffer else {
        log::error!("boot: framebuffer unavailable, idling");
        loop {
            core::hint::spin_loop();
        }
    };

    // Serial NDJSON tasks over native USB-Serial/JTAG on Core 0.
    let usb_serial =
        esp_hal::usb_serial_jtag::UsbSerialJtag::new(peripherals.USB_DEVICE).into_async();
    let (rx, tx) = usb_serial.split();

    // Hand HMI, display, and UI peripherals across to Core 1.
    let core1_peripherals = Core1Peripherals {
        spi2: peripherals.SPI2,
        dma_ch0: peripherals.DMA_CH0,
        pcnt: peripherals.PCNT,
        i2c0: peripherals.I2C0,
        ledc: peripherals.LEDC,
        gpio0: peripherals.GPIO0,
        gpio1: peripherals.GPIO1,
        gpio2: peripherals.GPIO2,
        gpio3: peripherals.GPIO3,
        gpio4: peripherals.GPIO4,
        gpio5: peripherals.GPIO5,
        gpio6: peripherals.GPIO6,
        gpio7: peripherals.GPIO7,
        gpio8: peripherals.GPIO8,
        gpio9: peripherals.GPIO9,
        gpio10: peripherals.GPIO10,
        gpio11: peripherals.GPIO11,
        gpio12: peripherals.GPIO12,
        gpio13: peripherals.GPIO13,
        gpio14: peripherals.GPIO14,
        gpio17: peripherals.GPIO17,
        fb_buf,
    };

    let parts = Core0Parts {
        cpu_ctrl: peripherals.CPU_CTRL,
        sw_int1: sw_int.software_interrupt1,
        core1_peripherals,
        rx,
        tx,
        wifi: peripherals.WIFI,
        bt: peripherals.BT,
        rng: peripherals.RNG,
        adc1: peripherals.ADC1,
    };

    let executor = CORE0_EXECUTOR.init(esp_rtos::embassy::Executor::new());
    executor.run_with_callbacks(
        |spawner| match core0_task(spawner, parts) {
            Ok(token) => spawner.spawn(token),
            Err(_) => log::error!("boot: failed to spawn core0_task"),
        },
        cpu_metrics::CoreTracker::new(&cpu_metrics::CPU0_USAGE),
    );
}
