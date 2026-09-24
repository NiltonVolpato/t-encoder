//! On-device isolation test for the PCNT/glitch-filter layer in isolation from
//! everything else (ISR wake mechanism, EVENTS channel, Slint dispatch — each
//! already verified clean independently).
//!
//! Drives a known, clean quadrature sequence directly into PCNT unit0 via two
//! spare GPIOs (GPIO15/GPIO16, looped back through their own
//! `Output::peripheral_input()` — no physical wiring, and the real encoder on
//! GPIO1/GPIO2 is never touched), using the exact same channel configuration
//! `EncoderHw::new` uses in production. Records the per-transition raw PCNT
//! delta pattern at a deliberately slow, safe pace, then replays the same
//! sequence at speeds approaching and exceeding realistic human dial-flick
//! rates, asserting every transition still produces the identical delta. Any
//! divergence at speed would point at the glitch filter or PCNT hardware
//! itself — not something a slow manual test with a real dial could ever
//! isolate from software timing/scheduling effects.

#![no_std]
#![no_main]

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

#[cfg(test)]
#[embedded_test::tests(executor = esp_rtos::embassy::Executor::new())]
mod tests {
    use defmt::assert_eq;
    use esp_hal::delay::Delay;
    use esp_hal::gpio::{Flex, Level, OutputConfig};
    use esp_hal::pcnt::Pcnt;
    use esp_hal::pcnt::channel::{CtrlMode, EdgeMode};
    use esp_hal::pcnt::unit::Unit;

    /// Matches `EncoderHw`'s `FILTER_THRESHOLD` in `bsp::rotary`.
    const FILTER_THRESHOLD: u16 = 1000;

    /// One full quadrature cycle, forward direction: (A, B) level per step.
    const FORWARD: [(bool, bool); 4] = [(false, false), (true, false), (true, true), (false, true)];

    /// Just the peripherals the test needs, handed from `init` to the test function
    /// (calling `esp_hal::init()` a second time inside the test would panic — it's
    /// a one-shot singleton).
    struct TestPeripherals {
        pcnt: esp_hal::peripherals::PCNT<'static>,
        gpio15: esp_hal::peripherals::GPIO15<'static>,
        gpio16: esp_hal::peripherals::GPIO16<'static>,
    }

    #[init]
    fn init() -> TestPeripherals {
        let peripherals = esp_hal::init(esp_hal::Config::default());
        esp_alloc::heap_allocator!(size: 64 * 1024);
        let timg0 = esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG0);
        let sw_interrupt =
            esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
        esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

        rtt_target::rtt_init_defmt!();

        TestPeripherals {
            pcnt: peripherals.PCNT,
            gpio15: peripherals.GPIO15,
            gpio16: peripherals.GPIO16,
        }
    }

    fn set_state(pin_a: &mut Flex, pin_b: &mut Flex, (a, b): (bool, bool)) {
        pin_a.set_level(if a { Level::High } else { Level::Low });
        pin_b.set_level(if b { Level::High } else { Level::Low });
    }

    /// Drives `cycles` full quadrature cycles (4 transitions each) in `dir`
    /// (1 = FORWARD order, -1 = reverse order), waiting `delay_us` between
    /// each transition, and returns the raw PCNT delta produced by every
    /// individual transition, in order.
    fn drive(
        unit: &Unit<'static, 0>,
        pin_a: &mut Flex,
        pin_b: &mut Flex,
        delay: &Delay,
        dir: i8,
        delay_us: u32,
        cycles: usize,
    ) -> alloc::vec::Vec<i32> {
        // Settle to the same canonical state (and let a full settling delay pass)
        // before recording anything, so every call's first recorded transition is
        // "leaving (false,false) after it's been held a while" regardless of what
        // ran before it — otherwise the first transition after a direction change
        // is a different electrical transition than a same-direction continuation,
        // and comparing them as if they were the same thing isn't valid.
        set_state(pin_a, pin_b, (false, false));
        delay.delay_micros(delay_us.max(2000));
        let mut deltas = alloc::vec::Vec::with_capacity(cycles * 4);
        let mut last = unit.value();
        for _ in 0..cycles {
            // step 1..=4, not 0..4: state is already at FORWARD[0] from the settle
            // above (or the previous cycle's last step), so the first real
            // transition out of it is step 1, and step 4 always lands back on
            // FORWARD[0], closing the cycle the same way every time.
            for step in 1..=4 {
                let idx = if dir > 0 { step % 4 } else { (4 - step) % 4 };
                set_state(pin_a, pin_b, FORWARD[idx]);
                delay.delay_micros(delay_us);
                let now = unit.value();
                deltas.push(i32::from(now.wrapping_sub(last)));
                last = now;
            }
        }
        deltas
    }

    #[test]
    async fn pcnt_never_diverges_from_slow_baseline_at_speed(peripherals: TestPeripherals) {
        let delay = Delay::new();

        let pcnt = Pcnt::new(peripherals.pcnt);
        let unit = pcnt.unit0;
        let _ = unit.set_filter(Some(FILTER_THRESHOLD));
        unit.clear();

        let mut pin_a = Flex::new(peripherals.gpio15);
        pin_a.apply_output_config(&OutputConfig::default());
        pin_a.set_low();
        pin_a.set_output_enable(true);

        let mut pin_b = Flex::new(peripherals.gpio16);
        pin_b.apply_output_config(&OutputConfig::default());
        pin_b.set_low();
        pin_b.set_output_enable(true);

        let sig_a = pin_a.peripheral_input();
        let sig_b = pin_b.peripheral_input();

        let ch0 = &unit.channel0;
        ch0.set_ctrl_signal(sig_a.clone());
        ch0.set_edge_signal(sig_b.clone());
        ch0.set_ctrl_mode(CtrlMode::Reverse, CtrlMode::Keep);
        ch0.set_input_mode(EdgeMode::Decrement, EdgeMode::Increment);

        let ch1 = &unit.channel1;
        ch1.set_ctrl_signal(sig_b);
        ch1.set_edge_signal(sig_a);
        ch1.set_ctrl_mode(CtrlMode::Reverse, CtrlMode::Keep);
        ch1.set_input_mode(EdgeMode::Increment, EdgeMode::Decrement);

        unit.resume();

        // Baseline: 2ms between transitions — far slower than any real dial
        // flick, safely above the glitch filter's ~12.5us threshold, used
        // only to record the expected per-step delta pattern.
        const BASELINE_US: u32 = 2000;
        let forward_baseline = drive(&unit, &mut pin_a, &mut pin_b, &delay, 1, BASELINE_US, 8);
        let reverse_baseline = drive(&unit, &mut pin_a, &mut pin_b, &delay, -1, BASELINE_US, 8);
        defmt::info!("forward baseline deltas: {}", &forward_baseline[..4]);
        defmt::info!("reverse baseline deltas: {}", &reverse_baseline[..4]);

        // Speeds from comfortably slow down toward (but not below) the glitch
        // filter threshold, covering realistic-to-very-fast human flick rates.
        const SPEEDS_US: [u32; 6] = [2000, 1000, 500, 200, 100, 50];
        const CYCLES_PER_SPEED: usize = 50;

        for &speed_us in &SPEEDS_US {
            let forward =
                drive(&unit, &mut pin_a, &mut pin_b, &delay, 1, speed_us, CYCLES_PER_SPEED);
            for (i, &d) in forward.iter().enumerate() {
                let expected = forward_baseline[i % 4];
                assert_eq!(
                    d, expected,
                    "forward @ {}us/step, transition {}: expected delta {}, got {}",
                    speed_us, i, expected, d
                );
            }

            let reverse =
                drive(&unit, &mut pin_a, &mut pin_b, &delay, -1, speed_us, CYCLES_PER_SPEED);
            for (i, &d) in reverse.iter().enumerate() {
                let expected = reverse_baseline[i % 4];
                assert_eq!(
                    d, expected,
                    "reverse @ {}us/step, transition {}: expected delta {}, got {}",
                    speed_us, i, expected, d
                );
            }

            defmt::info!(
                "{}us/step: {} transitions each direction, all matched baseline",
                speed_us,
                CYCLES_PER_SPEED * 4
            );
        }

        defmt::info!("pcnt_never_diverges_from_slow_baseline_at_speed: all speeds matched");
    }
}
