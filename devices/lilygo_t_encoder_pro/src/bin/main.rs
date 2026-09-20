#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use alloc::boxed::Box;
use app_launcher::LauncherAppFactory;
use app_shell::AppShell;
use defmt::info;
use embassy_executor::Spawner;
use esp_hal::clock::CpuClock;
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::timer::timg::TimerGroup;
use lilygo_t_encoder_pro::bsp::buzzer::buzzer_task;
use lilygo_t_encoder_pro::bsp::{
    Bsp, BspPeripherals, button_task, encoder_task, run_event_loop, touch_task,
};
use panic_rtt_target as _;

extern crate alloc;

// Creates a default app-descriptor required by the esp-idf bootloader.
esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    rtt_target::rtt_init_defmt!();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // 1. Initialize PSRAM (Region 0 - default general allocator)
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    // 2. Register internal DRAM heaps (DMA buffers & fast RAM)
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    esp_alloc::heap_allocator!(size: 64 * 1024);

    // 3. Initialize RTOS & Embassy tick driver
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    info!("Memory & RTOS initialized, bringing up BSP...");

    // 4. Initialize BSP (display, touch, rotary, Slint platform)
    let bsp = Bsp::init(BspPeripherals {
        spi2: peripherals.SPI2,
        dma_ch0: peripherals.DMA_CH0,
        i2c0: peripherals.I2C0,
        pcnt: peripherals.PCNT,
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
        timg1: peripherals.TIMG1,
    });

    // 5. Spawn background buzzer & haptics task on GPIO17
    spawner.spawn(
        buzzer_task(peripherals.LEDC, peripherals.GPIO17).expect("Failed to create buzzer task"),
    );

    // 6. Spawn interrupt-driven input tasks
    spawner.spawn(encoder_task(bsp.encoder_hw).expect("Failed to create encoder task"));
    spawner.spawn(button_task(bsp.button).expect("Failed to create button task"));
    if let Some(touch) = bsp.touch {
        spawner.spawn(touch_task(touch).expect("Failed to create touch task"));
    }

    info!("Starting AppShell with Launcher...");
    let launcher = LauncherAppFactory::default_apps();
    let shell = AppShell::new(Box::new(launcher));
    AppShell::start(&shell);

    // 7. Enter Slint MCU event loop
    run_event_loop(bsp.window, bsp.display).await;
}
