//! Demo test suite using embedded-test
//!
//! You can run this using `cargo test` as usual.

#![no_std]
#![no_main]

esp_bootloader_esp_idf::esp_app_desc!();

#[cfg(test)]
#[embedded_test::tests(executor = esp_rtos::embassy::Executor::new())]
mod tests {
    use defmt::assert_eq;

    #[init]
    fn init() {
        let peripherals = esp_hal::init(esp_hal::Config::default());

        let timg0 = esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG0);
        let sw_interrupt =
            esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
        esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

        rtt_target::rtt_init_defmt!();
    }

    #[test]
    async fn hello_test() {
        defmt::info!("Running basic sanity test!");
        embassy_time::Timer::after(embassy_time::Duration::from_millis(50)).await;
        assert_eq!(1 + 1, 2);
    }

    #[test]
    async fn test_native_rgb565_fill_slice() {
        use lilygo_t_encoder_pro::bsp::display::NativeRgb565;
        use slint::platform::software_renderer::TargetPixel;

        let mut buf = [NativeRgb565::new(0); 35];
        NativeRgb565::fill_slice(&mut buf[1..33], NativeRgb565::new(0xBEEF));
        assert_eq!(buf[0].raw(), 0);
        for p in &buf[1..33] {
            assert_eq!(p.raw(), 0xBEEF);
        }
        assert_eq!(buf[33].raw(), 0);
        assert_eq!(buf[34].raw(), 0);

        defmt::info!("test_native_rgb565_fill_slice passed!");
    }

    #[test]
    async fn test_expand_2x2_scale2x() {
        use lilygo_t_encoder_pro::bsp::display::{NativeRgb565, expand_2x2_chunk};

        // 3x3 pattern in a 3-wide stride:
        // [1, 1, 2]
        // [1, 0, 2]
        // [3, 3, 2]
        // Center pixel at (1, 1) is P=0.
        // Neighbors: U=1, D=3, L=1, R=2.
        // Scale2x:
        // U != D (1 != 3) and L != R (1 != 2):
        // E0 = (L == U ? U : P) = (1 == 1 ? 1 : 0) = 1
        // E1 = (R == U ? U : P) = (2 == 1 ? 1 : 0) = 0
        // E2 = (L == D ? D : P) = (1 == 3 ? 3 : 0) = 0
        // E3 = (R == D ? D : P) = (2 == 3 ? 3 : 0) = 0
        let p = |raw: u16| NativeRgb565::new(raw);
        let fb = [p(1), p(1), p(2), p(1), p(0), p(2), p(3), p(3), p(2)];

        // Output buffer for 1 row of width 3 -> 2 physical rows * 3 words = 6 words = 24 bytes
        #[repr(align(4))]
        struct AlignedBuf([u8; 24]);
        let mut out = AlignedBuf([0; 24]);

        let used = expand_2x2_chunk(&fb, 3, 0, 1, 3, 1, &mut out.0);
        assert_eq!(used, 24);

        let e0_be = 1u16.to_be();
        let e1_be = 0u16.to_be();
        let e2_be = 0u16.to_be();
        let e3_be = 0u16.to_be();

        // Row 0, col 1:
        assert_eq!(out.0[4], (e0_be & 0xFF) as u8);
        assert_eq!(out.0[5], (e0_be >> 8) as u8);
        assert_eq!(out.0[6], (e1_be & 0xFF) as u8);
        assert_eq!(out.0[7], (e1_be >> 8) as u8);

        // Row 1, col 1 (offset: 3 words * 4 bytes = 12 bytes + 4 = 16):
        assert_eq!(out.0[16], (e2_be & 0xFF) as u8);
        assert_eq!(out.0[17], (e2_be >> 8) as u8);
        assert_eq!(out.0[18], (e3_be & 0xFF) as u8);
        assert_eq!(out.0[19], (e3_be >> 8) as u8);

        defmt::info!("test_expand_2x2_scale2x passed!");
    }
}
