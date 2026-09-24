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
use esp_hal::interrupt::software::{SoftwareInterrupt, SoftwareInterruptControl};
use esp_hal::system::Stack;
use esp_hal::timer::timg::TimerGroup;
use panic_rtt_target as _;
use waveshare_knob_1_8::bsp::{
    Board, Bsp, Core1Peripherals, Sh8601, display_task, haptic_task, rotary_task, run_event_loop,
    touch_task,
};
use waveshare_knob_1_8::tasks::{PROFILER_ENABLED, profiler_task, screensaver_task};

extern crate alloc;

defmt::timestamp!("{=u32:us}", { xtensa_lx::timer::get_cycle_count() / 240 });

esp_bootloader_esp_idf::esp_app_desc!();

/// Core 1 execution stack (16 KiB, 16-byte aligned).
const APP_CORE_STACK_SIZE: usize = 16 * 1024;
static mut APP_CORE_STACK: Stack<APP_CORE_STACK_SIZE> = Stack::new();

/// Entry point for Core 1 (`AppCpu`): owns SH8601 display initialization and QSPI DMA transfers.
fn core1_entry(core1: Core1Peripherals, sw_int2: SoftwareInterrupt<'static, 2>) -> ! {
    let display = Sh8601::new(core1.display);

    let interrupt_executor =
        Box::leak(Box::new(esp_rtos::embassy::InterruptExecutor::new(sw_int2)));
    let send_spawner = interrupt_executor.start(esp_hal::interrupt::Priority::Priority1);
    send_spawner.spawn(display_task(display).expect("Failed to create display task"));

    let executor = Box::leak(Box::new(esp_rtos::embassy::Executor::new()));
    executor.run(|_spawner| {});
}

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
    info!("{}", esp_alloc::HEAP.stats());

    // 3. Initialize RTOS & Embassy tick driver
    let timg0 = TimerGroup::new(board.system.timg0);
    let sw_int = SoftwareInterruptControl::new(board.system.sw_interrupt);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    // 4. Start Core 1 (AppCpu) for display and QSPI DMA transfers
    esp_rtos::start_second_core::<APP_CORE_STACK_SIZE>(
        board.system.cpu_ctrl,
        sw_int.software_interrupt1,
        unsafe { &mut *core::ptr::addr_of_mut!(APP_CORE_STACK) },
        move || {
            core1_entry(board.core1, sw_int.software_interrupt2);
        },
    );

    info!("Memory & RTOS initialized, bringing up BSP on Core 0...");

    // 5. Initialize BSP on Core 0 (touch, rotary, Slint platform, profiler)
    let bsp = Bsp::init(board.core0);

    // 6. Spawn interrupt-driven input, haptics & screensaver tasks
    spawner.spawn(rotary_task().expect("Failed to create rotary task"));
    if let Some(touch) = bsp.touch {
        spawner.spawn(touch_task(touch).expect("Failed to create touch task"));
    }
    if let Some(haptic_i2c) = bsp.haptic_i2c {
        spawner.spawn(haptic_task(haptic_i2c).expect("Failed to create haptic task"));
    }
    spawner.spawn(screensaver_task().expect("Failed to create screensaver task"));
    if PROFILER_ENABLED {
        spawner.spawn(profiler_task(bsp.profiler_timer).expect("Failed to create profiler task"));
    }

    info!("Starting AppShell with Launcher on Core 0...");
    let launcher = LauncherAppFactory::default_apps();
    let shell = AppShell::new(Box::new(launcher));
    AppShell::start(&shell);

    // 7. Enter Slint MCU event loop on Core 0
    run_event_loop(bsp.window).await;
}
