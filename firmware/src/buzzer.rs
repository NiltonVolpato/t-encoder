// Adapted from rust-enc (https://github.com/rayslava/rust-enc), MIT licensed.
// Copyright (c) rayslava. Copied from enc-app, which is a binary crate and so
// cannot be reached by path dependency; diverges from upstream from here on.

//! GPIO17 transducer feedback: an audible beep and a low-frequency haptic
//! vibration, driven by two LEDC low-speed timers on the one shared channel.
//!
//! Events are delivered through a small [`Channel`] (not a single-slot
//! `Signal`) so an alarm [`Feedback::Haptic`] can never be overwritten by a
//! [`Feedback::Beep`] while the task is mid-pulse.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use esp_hal::gpio::DriveMode;
use esp_hal::ledc::channel::{self, ChannelIFace};
use esp_hal::ledc::timer::{self, TimerIFace};
use esp_hal::ledc::{LSGlobalClkSource, Ledc, LowSpeed};
use esp_hal::peripherals::{GPIO17, LEDC};
use esp_hal::time::Rate;

/// Feedback on the single GPIO17 transducer.
#[derive(Clone, Copy)]
pub enum Feedback {
    /// Short audible click for input acknowledgement.
    Beep,
    /// Vibration buzz (e.g. an alarm firing).
    Haptic,
}

/// Queued feedback requests, served by [`task`]. Depth keeps a burst of input
/// beeps from dropping a pending alarm haptic.
static FEEDBACK: Channel<CriticalSectionRawMutex, Feedback, 4> = Channel::new();

/// Enqueues a feedback event, dropping it only if the queue is full.
pub fn signal(feedback: Feedback) {
    let _ = FEEDBACK.try_send(feedback);
}

/// Drives the GPIO17 transducer. Two LEDC timers give the audible beep and the
/// low-frequency vibration their distinct carriers; the single channel is
/// rebound to the right timer per request.
#[embassy_executor::task]
pub async fn task(ledc_periph: LEDC<'static>, pin: GPIO17<'static>) {
    let mut ledc = Ledc::new(ledc_periph);
    ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);

    // 13-bit duty so the low haptic carrier (200 Hz) is reachable: the LEDC
    // minimum frequency is clock/(max_div * 2^bits), which 8-bit can't hit at
    // 200 Hz (~305 Hz floor) but 13-bit easily can (~9.5 Hz floor).
    let make_timer = |ledc: &Ledc<'static>, number, hz| {
        let mut timer = ledc.timer::<LowSpeed>(number);
        timer
            .configure(timer::config::Config {
                duty: timer::config::Duty::Duty13Bit,
                clock_source: timer::LSClockSource::APBClk,
                frequency: Rate::from_hz(hz),
            })
            .map(|()| timer)
    };
    let (Ok(beep_timer), Ok(haptic_timer)) = (
        make_timer(
            &ledc,
            timer::Number::Timer0,
            enc_config::buzzer::FREQUENCY_HZ,
        ),
        make_timer(
            &ledc,
            timer::Number::Timer1,
            enc_config::buzzer::HAPTIC_FREQUENCY_HZ,
        ),
    ) else {
        log::error!("buzzer: timer config failed");
        return;
    };

    let mut chan = ledc.channel(channel::Number::Channel0, pin);

    // Drives the transducer for `ms` at `duty` percent using the given timer.
    macro_rules! pulse {
        ($timer:expr, $duty:expr, $ms:expr) => {{
            if chan
                .configure(channel::config::Config {
                    timer: $timer,
                    duty_pct: 0,
                    drive_mode: DriveMode::PushPull,
                })
                .is_ok()
            {
                let _ = chan.set_duty($duty);
                Timer::after(Duration::from_millis($ms)).await;
                let _ = chan.set_duty(0);
            }
        }};
    }

    // Boot beep, then serve feedback requests.
    pulse!(&beep_timer, 50, 80);
    loop {
        match FEEDBACK.receive().await {
            Feedback::Beep => pulse!(&beep_timer, 50, 35),
            Feedback::Haptic => pulse!(&haptic_timer, 70, 250),
        }
    }
}
