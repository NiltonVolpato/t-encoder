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
    Board, Bsp, button_task, display_task, encoder_task, run_event_loop, touch_task,
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
    let board = Board::new(peripherals);

    // 1. Initialize PSRAM (Region 0 - default general allocator)
    esp_alloc::psram_allocator!(board.system.psram, esp_hal::psram);

    // 2. Register internal DRAM heaps (DMA buffers & fast RAM)
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    esp_alloc::heap_allocator!(size: 64 * 1024);

    // 3. Initialize RTOS & Embassy tick driver
    let timg0 = TimerGroup::new(board.system.timg0);
    let sw_int = SoftwareInterruptControl::new(board.system.sw_interrupt);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    info!("Memory & RTOS initialized, bringing up BSP...");

    // 4. Initialize BSP (display, touch, rotary, Slint platform)
    let bsp = Bsp::init(board.core0, board.core1.display);

    // 5. Spawn background buzzer & haptics task on GPIO17
    spawner.spawn(
        buzzer_task(bsp.buzzer.ledc, bsp.buzzer.pin).expect("Failed to create buzzer task"),
    );

    // 7. Spawn interrupt-driven input tasks
    spawner.spawn(encoder_task(bsp.encoder_hw).expect("Failed to create encoder task"));
    spawner.spawn(button_task(bsp.button).expect("Failed to create button task"));
    if let Some(touch) = bsp.touch {
        spawner.spawn(touch_task(touch).expect("Failed to create touch task"));
    }

    info!("Starting AppShell with Launcher...");
    let launcher = LauncherAppFactory::default_apps();
    let shell = AppShell::new(Box::new(launcher));
    AppShell::start(&shell);

    // 8. Spawn display worker task on InterruptExecutor at Priority1
    let interrupt_executor = Box::leak(Box::new(esp_rtos::embassy::InterruptExecutor::new(
        sw_int.software_interrupt1,
    )));
    let send_spawner = interrupt_executor.start(esp_hal::interrupt::Priority::Priority1);
    send_spawner
        .spawn(display_task(bsp.display).expect("Failed to create display task"));

    // 9. Enter Slint MCU event loop
    run_event_loop(bsp.window).await;
}
