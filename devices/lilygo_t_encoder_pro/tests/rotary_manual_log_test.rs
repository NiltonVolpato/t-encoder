//! Manual, human-validated test of the real rotary encoder (`bsp::rotary`),
//! wired exactly as production does — real GPIO1/GPIO2, real PCNT, real
//! `rotary_isr` — with nothing else running: no Slint, no touch, no buzzer,
//! no display, no render load. Just the encoder and a trivial event-loop
//! stand-in that drains `EVENTS` and logs whatever arrives.
//!
//! This does not assert anything. It prints an alternating "turn RIGHT" /
//! "turn LEFT" prompt at a fixed interval so you have a known reference
//! sequence to follow, and logs every rotary event as it happens (both from
//! the ISR directly and from the EVENTS channel consumer, so a mismatch
//! between the two would itself be visible). Redirect the run to a file and
//! compare the logged sequence against what you actually did by hand:
//!
//!   cargo test --test rotary_manual_log_test 2>&1 | tee rotary_manual_log.txt
//!
//! If this — real encoder, zero other system load — still shows a skip or a
//! double, that's independent of Slint/render/touch contention entirely. If
//! it's clean here despite being reproducible with the full app running,
//! that points at something in the loaded system, not the encoder itself.

#![no_std]
#![no_main]

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

#[cfg(test)]
#[embedded_test::tests(executor = esp_rtos::embassy::Executor::new())]
mod tests {
    use embassy_futures::select::{Either3, select3};
    use embassy_time::{Duration, Timer};
    use lilygo_t_encoder_pro::bsp::{
        EVENTS, EncoderHw, Event, InputEvent, init_rotary, rotary_decode_once,
    };

    #[init]
    fn init() {
        let peripherals = esp_hal::init(esp_hal::Config::default());

        let timg0 = esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG0);
        let sw_interrupt =
            esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
        esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

        rtt_target::rtt_init_defmt!();

        // Real encoder, real pins, real PCNT config, real ISR — identical to
        // what `Bsp::init` wires up in production.
        let encoder_hw = EncoderHw::new(peripherals.PCNT, peripherals.GPIO1, peripherals.GPIO2);
        init_rotary(peripherals.IO_MUX, encoder_hw);
    }

    /// Total number of prompts to show (24 * 5s = 2 minutes).
    const PROMPT_COUNT: u32 = 24;

    fn direction_for(round: u32) -> &'static str {
        if round < 5 {
            "RIGHT"
        } else if round < 10 {
            "LEFT"
        } else if round < 15 {
            "REVERSE"
        } else {
            "ANY DIRECTION"
        }
    }

    #[test]
    #[timeout(180)]
    async fn manual_rotate_encoder_only_no_app_load() {
        defmt::info!(
            "=== Manual rotary test: real encoder, no Slint/touch/buzzer/display running ==="
        );
        defmt::info!(
            "Turn the dial ONE detent per prompt, in the direction shown, and wait for the next prompt."
        );
        defmt::info!(
            "Every real rotary event logs itself (see 'rotary detents: raw=... delta=...' lines)."
        );
        defmt::info!(
            "This test does not check anything automatically — compare the log against what you did, by eye, afterward."
        );

        let mut round = 1u32;
        loop {
            // Drive one settle-and-decode round inline (production spawns
            // `rotary_task` instead); it parks until a pin edge arrives.
            // `Either3::Third` means a decode round completed silently.
            match select3(
                EVENTS.receive(),
                Timer::after(Duration::from_secs(4)),
                rotary_decode_once(),
            )
            .await
            {
                Either3::First(event) => match event {
                    Event::Input(InputEvent::Rotate(delta)) => {
                        defmt::info!("EVENTS: Rotate({})", delta);
                    }
                    Event::Input(other) => {
                        defmt::info!("EVENTS: other input {}", defmt::Debug2Format(&other));
                    }
                    Event::Screen(_) => {}
                },
                Either3::Second(()) => {
                    round += 1;
                    if round > PROMPT_COUNT {
                        break;
                    }
                    defmt::info!(
                        "[prompt {}/{}] Turn {} now",
                        round,
                        PROMPT_COUNT,
                        direction_for(round)
                    );
                }
                Either3::Third(()) => {}
            }
        }

        defmt::info!(
            "=== Manual rotary test complete: {} prompts shown ===",
            PROMPT_COUNT
        );
    }
}
