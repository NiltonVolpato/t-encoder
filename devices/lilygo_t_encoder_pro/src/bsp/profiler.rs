// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Statistical sampling profiler for Xtensa LX7 (ESP32-S3).
//!
//! Samples the program counter (PC) from `EPC1` at 2 kHz using `TIMG1.timer0`.
//! Sampling is armed on the first input event and runs for 10 seconds, only
//! recording samples during active frame rendering (`renderer.render()`).

use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, Ordering};
use critical_section::Mutex;
use defmt::info;
use esp_hal::Blocking;
use esp_hal::handler;
use esp_hal::peripherals::TIMG1;
use esp_hal::time::{Duration, Instant};
use esp_hal::timer::PeriodicTimer;
use esp_hal::timer::timg::TimerGroup;

use app_shell::profile::SampleTable;

pub const SAMPLING_PERIOD: Duration = Duration::from_micros(500); // 2 kHz
pub const CAPTURE_DURATION: Duration = Duration::from_secs(10);

static IS_ARMED: AtomicBool = AtomicBool::new(false);
static IS_RENDERING: AtomicBool = AtomicBool::new(false);
static IS_FINISHED: AtomicBool = AtomicBool::new(false);

static CAPTURE_START: Mutex<RefCell<Option<Instant>>> = Mutex::new(RefCell::new(None));
static TIMER: Mutex<RefCell<Option<PeriodicTimer<'static, Blocking>>>> =
    Mutex::new(RefCell::new(None));
static SAMPLES: Mutex<RefCell<SampleTable<128>>> = Mutex::new(RefCell::new(SampleTable::new()));

#[handler]
fn profiler_isr() {
    critical_section::with(|cs| {
        if let Some(timer) = TIMER.borrow_ref_mut(cs).as_mut() {
            timer.clear_interrupt();
        }
    });

    if !IS_ARMED.load(Ordering::Relaxed)
        || !IS_RENDERING.load(Ordering::Relaxed)
        || IS_FINISHED.load(Ordering::Relaxed)
    {
        return;
    }

    let pc: u32;
    unsafe {
        core::arch::asm!("rsr.epc1 {0}", out(reg) pc);
    }

    critical_section::with(|cs| {
        SAMPLES.borrow_ref_mut(cs).record(pc);
    });
}

/// Initializes the profiler hardware timer and registers the 2 kHz interrupt handler.
pub fn init(timg1: TIMG1<'static>) {
    let timg = TimerGroup::new(timg1);
    let mut timer = PeriodicTimer::new(timg.timer0);
    timer.set_interrupt_handler(profiler_isr);
    timer.listen();
    if let Err(e) = timer.start(SAMPLING_PERIOD) {
        defmt::error!("Failed to start profiler timer: {:?}", defmt::Debug2Format(&e));
        return;
    }

    critical_section::with(|cs| {
        TIMER.borrow_ref_mut(cs).replace(timer);
    });

    info!("Statistical profiler initialized (2 kHz sampling, armed on first input event).");
}

/// Call when an input event is received. Arms the profiler on the first input.
pub fn on_input_event() {
    if !IS_ARMED.load(Ordering::Relaxed) && !IS_FINISHED.load(Ordering::Relaxed) {
        critical_section::with(|cs| {
            CAPTURE_START.borrow_ref_mut(cs).replace(Instant::now());
        });
        IS_ARMED.store(true, Ordering::Relaxed);
        info!("[PROFILE] Started 10-second sampling window on first input event!");
    }
}

/// Call immediately before invoking `renderer.render()`.
pub fn start_render() {
    IS_RENDERING.store(true, Ordering::Relaxed);
}

/// Call immediately after `renderer.render()` completes.
pub fn stop_render() {
    IS_RENDERING.store(false, Ordering::Relaxed);
}

/// Polls the profiler timer. Once the 10-second window expires, logs the report.
pub fn poll() {
    if !IS_ARMED.load(Ordering::Relaxed) || IS_FINISHED.load(Ordering::Relaxed) {
        return;
    }

    let started_at = critical_section::with(|cs| *CAPTURE_START.borrow_ref(cs));
    if let Some(start) = started_at {
        if Instant::now() - start >= CAPTURE_DURATION {
            IS_ARMED.store(false, Ordering::Relaxed);
            IS_FINISHED.store(true, Ordering::Relaxed);

            dump_report();
        }
    }
}

fn dump_report() {
    critical_section::with(|cs| {
        let table = SAMPLES.borrow_ref(cs);
        let total = table.total_samples();
        let unique = table.unique_count();

        info!("================================================================");
        info!("[PROFILE] 10-Second Render Profile Complete!");
        info!(
            "[PROFILE] Total render samples: {} (across {} unique PCs)",
            total, unique
        );

        if total > 0 {
            let (top, count) = table.top_samples::<15>();
            info!("[PROFILE] Top {} Hotspots:", count);
            for (i, entry) in top[..count].iter().enumerate() {
                let pct = (entry.count as f32 / total as f32) * 100.0;
                info!(
                    "[PROFILE]   #{}: 0x{:08x} - {} samples ({}%)",
                    i + 1,
                    entry.pc,
                    entry.count,
                    pct
                );
            }
            info!("[PROFILE] To symbolize, run: python3 tools/symbolize_profile.py");
        } else {
            info!("[PROFILE] No render samples captured during the 10s window.");
        }
        info!("================================================================");
    });
}
