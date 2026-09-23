// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Transducer buzzer & haptics driver using a single dynamic LEDC timer on GPIO17.

use core::cell::RefCell;

pub use app_shell::Feedback;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use esp_hal::gpio::DriveMode;
use esp_hal::ledc::channel::{self, ChannelIFace};
use esp_hal::ledc::timer::{self, TimerIFace};
use esp_hal::ledc::{LSGlobalClkSource, Ledc, LowSpeed};
use esp_hal::peripherals::{GPIO17, LEDC};
use esp_hal::time::Rate;

/// Signal used to park and wake the buzzer task with zero CPU polling when idle.
static BUZZER_WAKER: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Queues a feedback event and wakes the parked buzzer task to play it.
///
/// This is the only entry point callers should use. `app_shell::feedback::signal`
/// merely queues the event (it's platform-agnostic and doesn't know about
/// `BUZZER_WAKER`); calling it directly would leave the event queued but the
/// buzzer task parked until something else happens to wake it.
pub fn signal_feedback(feedback: Feedback) {
    if !app_shell::feedback::signal(feedback) {
        defmt::error!("feedback queue full, dropped {}", defmt::Debug2Format(&feedback));
    }
    BUZZER_WAKER.signal(());
}

/// A wrapper around `esp_hal::ledc::timer::Timer` that provides interior mutability
/// so its frequency can be changed on the fly while bound to an LEDC channel.
struct DynamicTimer<'a>(RefCell<esp_hal::ledc::timer::Timer<'a, LowSpeed>>);

impl TimerIFace<LowSpeed> for DynamicTimer<'_> {
    fn freq(&self) -> Option<Rate> {
        self.0.borrow().freq()
    }

    fn configure(
        &mut self,
        config: esp_hal::ledc::timer::config::Config<timer::LSClockSource>,
    ) -> Result<(), esp_hal::ledc::timer::Error> {
        self.0.borrow_mut().configure(config)
    }

    fn is_configured(&self) -> bool {
        self.0.borrow().is_configured()
    }

    fn duty(&self) -> Option<esp_hal::ledc::timer::config::Duty> {
        self.0.borrow().duty()
    }

    fn number(&self) -> esp_hal::ledc::timer::Number {
        self.0.borrow().number()
    }

    fn frequency(&self) -> u32 {
        self.0.borrow().frequency()
    }
}

impl DynamicTimer<'_> {
    fn set_frequency(&self, hz: u32) {
        let _ = self.0.borrow_mut().configure(esp_hal::ledc::timer::config::Config {
            duty: esp_hal::ledc::timer::config::Duty::Duty13Bit,
            clock_source: timer::LSClockSource::APBClk,
            frequency: Rate::from_hz(hz),
        });
    }
}

/// Asynchronous Embassy task serving buzzer & haptic requests.
#[embassy_executor::task]
pub async fn buzzer_task(ledc_periph: LEDC<'static>, pin: GPIO17<'static>) {
    let mut ledc = Ledc::new(ledc_periph);
    ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);

    let raw_timer = ledc.timer::<LowSpeed>(timer::Number::Timer0);
    let timer = DynamicTimer(RefCell::new(raw_timer));
    timer.set_frequency(1100);

    let mut chan = ledc.channel(channel::Number::Channel0, pin);
    let _ = chan.configure(channel::config::Config {
        timer: &timer,
        duty_pct: 0,
        drive_mode: DriveMode::PushPull,
    });

    macro_rules! pulse {
        ($hz:expr, $duty:expr, $ms:expr) => {{
            let hz: u32 = $hz;
            let ms: u32 = $ms;
            if hz > 0 && ms > 0 {
                timer.set_frequency(hz);
                let _ = chan.set_duty($duty);
                Timer::after(Duration::from_millis(ms as u64)).await;
                let _ = chan.set_duty(0);
            }
        }};
    }

    // Initial boot beep (1,100 Hz for 80ms)
    pulse!(1100, 50, 80);

    loop {
        while let Some(feedback) = app_shell::feedback::try_receive() {
            match feedback {
                Feedback::DialStepForward => {
                    // Ascending two-tone chirp (523 Hz -> 659 Hz)
                    pulse!(523, 50, 25);
                    pulse!(659, 50, 25);
                }
                Feedback::DialStepBackward => {
                    // Descending two-tone chirp (659 Hz -> 523 Hz)
                    pulse!(659, 50, 25);
                    pulse!(523, 50, 25);
                }
                Feedback::Click => {
                    // Crisp blip (880 Hz for 15ms)
                    pulse!(880, 50, 15);
                }
                Feedback::Haptic => {
                    // Low-frequency vibration buzz (200 Hz, 70% duty for 250ms)
                    pulse!(200, 70, 250);
                }
                Feedback::Tone { hz, ms } => {
                    pulse!(hz, 50, ms);
                }
            }
        }
        // Park task with 0% CPU consumption until next signal arrives
        BUZZER_WAKER.wait().await;
    }
}
