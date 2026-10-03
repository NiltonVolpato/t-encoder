//! Benchmark comparing SIMD fill (`simd_fill_color`) against scalar u32-unrolled fill.
//!
//! Run on device with:
//!   cargo test -p lilygo_t_encoder_pro --test fill_benchmark --release

#![no_std]
#![no_main]

esp_bootloader_esp_idf::esp_app_desc!();

#[cfg(test)]
#[embedded_test::tests(executor = esp_rtos::embassy::Executor::new())]
mod tests {
    use defmt::info;
    use lilygo_t_encoder_pro::bsp::simd;
    use xtensa_lx::timer::get_cycle_count;

    const FILL_VALUE: u16 = 0xBEEF;
    /// Largest slice we benchmark, plus headroom for 16-byte alignment offsets.
    const MAX_PIXELS: usize = 16_384;
    const RUNS: usize = 50;

    /// 16-byte aligned backing store so the SIMD path gets its required alignment.
    #[repr(C, align(16))]
    struct AlignedBuf([u16; MAX_PIXELS + 8]);

    static mut BUF: AlignedBuf = AlignedBuf([0; MAX_PIXELS + 8]);

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
    async fn benchmark_fill_slice_variants() {
        simd::enable_pie();

        // Correctness smoke test before timing.
        verify_fill("simd", |slice| simd::fill_slice(slice, FILL_VALUE));
        verify_fill("scalar u32", |slice| simd::fill_slice_scalar_u32(slice, FILL_VALUE));

        let offsets_bytes = [0usize, 2, 4, 8, 12];
        let sizes = [32usize, 128, 256, 512, 1024, 2048, 4096, 8192];

        info!("fill benchmark: cycles per pixel (best of {} runs)", RUNS);
        info!("cpp_1k = cycles per 1024 pixels; ratio_1k = scalar/simd (1024=equal)",);
        info!("offset size simd_cpp_1k scalar_cpp_1k ratio_1k");

        let buf = unsafe { &mut (*core::ptr::addr_of_mut!(BUF)).0 };
        for &offset_bytes in &offsets_bytes {
            for &size in &sizes {
                let offset_pixels = offset_bytes / 2;
                let slice = &mut buf[offset_pixels..offset_pixels + size];

                let simd_cycles = bench_best(slice, |s| simd::fill_slice(s, FILL_VALUE));
                let scalar_cycles =
                    bench_best(slice, |s| simd::fill_slice_scalar_u32(s, FILL_VALUE));

                // Cycles per 1024 pixels (fixed-point, avoids float formatting).
                let simd_cpp_1k = (simd_cycles as u64 * 1024) / size as u64;
                let scalar_cpp_1k = (scalar_cycles as u64 * 1024) / size as u64;
                let ratio_1k = (scalar_cycles as u64 * 1024) / simd_cycles.max(1) as u64;

                info!("{} {} {} {} {}", offset_bytes, size, simd_cpp_1k, scalar_cpp_1k, ratio_1k);
            }
        }
    }

    fn verify_fill<F>(name: &str, fill: F)
    where
        F: Fn(&mut [u16]),
    {
        let buf = unsafe { &mut (*core::ptr::addr_of_mut!(BUF)).0 };
        let slice = &mut buf[0..64];
        fill(slice);

        for &word in slice.iter() {
            defmt::assert_eq!(word, FILL_VALUE);
        }
        info!("{} fill correctness ok", name);
    }

    fn bench_best<F>(slice: &mut [u16], fill: F) -> u32
    where
        F: Fn(&mut [u16]),
    {
        let mut best = u32::MAX;
        for _ in 0..RUNS {
            let start = get_cycle_count();
            fill(slice);
            let end = get_cycle_count();
            let cycles = end.wrapping_sub(start);
            if cycles < best {
                best = cycles;
            }
        }
        best
    }
}
