//! Dual-core CPU utilization tracking using `esp_rtos::embassy::Callbacks`.
//!
//! Measures active vs idle cycles using the hardware Xtensa cycle counter (`CCOUNT`).
//! Computes rolling 1-second CPU usage percentages for Core 0 (network/system) and Core 1 (UI/display).

use core::sync::atomic::{AtomicU8, Ordering};
use esp_rtos::embassy::Callbacks;

/// 1 second of cycles at 240 MHz.
const WINDOW_CYCLES: u32 = 240_000_000;

/// Core 0 (`ProCpu`) rolling CPU usage percentage (0..100%).
pub static CPU0_USAGE: AtomicU8 = AtomicU8::new(0);

/// Core 1 (`AppCpu`) rolling CPU usage percentage (0..100%).
pub static CPU1_USAGE: AtomicU8 = AtomicU8::new(0);

/// Reads the hardware 32-bit cycle counter (`CCOUNT`).
#[inline]
#[must_use]
pub fn get_cycle_count() -> u32 {
    #[cfg(target_arch = "xtensa")]
    {
        esp_hal::xtensa_lx::timer::get_cycle_count()
    }
    #[cfg(not(target_arch = "xtensa"))]
    {
        0
    }
}

/// Returns the current rolling CPU usage percentage for Core 0.
#[must_use]
pub fn cpu0_usage_pct() -> u8 {
    CPU0_USAGE.load(Ordering::Relaxed)
}

/// Returns the current rolling CPU usage percentage for Core 1.
#[must_use]
pub fn cpu1_usage_pct() -> u8 {
    CPU1_USAGE.load(Ordering::Relaxed)
}

/// Callbacks tracker passed to `Executor::run_with_callbacks`.
pub struct CoreTracker {
    last_ts: u32,
    window_start_ts: u32,
    active_accum: u32,
    idle_accum: u32,
    target: &'static AtomicU8,
}

impl CoreTracker {
    /// Creates a new tracker updating `target` with 1-second rolling CPU percentage.
    #[must_use]
    pub fn new(target: &'static AtomicU8) -> Self {
        let now = get_cycle_count();
        Self {
            last_ts: now,
            window_start_ts: now,
            active_accum: 0,
            idle_accum: 0,
            target,
        }
    }

    #[inline]
    fn check_window(&mut self, now: u32) {
        let window_delta = now.wrapping_sub(self.window_start_ts);
        if window_delta >= WINDOW_CYCLES {
            let active = u64::from(self.active_accum);
            let idle = u64::from(self.idle_accum);
            let total = active.saturating_add(idle);
            if let Some(pct_u64) = active.saturating_mul(100).checked_div(total) {
                let pct = u8::try_from(pct_u64.min(100)).unwrap_or(100);
                self.target.store(pct, Ordering::Relaxed);
            }
            self.active_accum = 0;
            self.idle_accum = 0;
            self.window_start_ts = now;
        }
    }
}

impl Callbacks for CoreTracker {
    #[inline]
    fn before_poll(&mut self) {
        let now = get_cycle_count();
        let idle_delta = now.wrapping_sub(self.last_ts);
        self.idle_accum = self.idle_accum.wrapping_add(idle_delta);
        self.last_ts = now;
        self.check_window(now);
    }

    #[inline]
    fn on_idle(&mut self) {
        let now = get_cycle_count();
        let active_delta = now.wrapping_sub(self.last_ts);
        self.active_accum = self.active_accum.wrapping_add(active_delta);
        self.last_ts = now;
        self.check_window(now);
    }
}
