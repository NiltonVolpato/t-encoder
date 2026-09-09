use std::env;
use std::time::{Duration, Instant};

use espielberg::Director;

fn main() {
    let port = env::var("FLASH_PORT").unwrap_or_else(|_| "/dev/cu.usbmodem101".to_string());
    let count: usize = env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(5);

    println!("Connecting to {port}...");
    let mut director = match Director::action(&port) {
        Ok(d) => d,
        Err(err) => {
            eprintln!("Failed to connect to device on {port}: {err}");
            std::process::exit(1);
        }
    };

    println!("Taking {count} consecutive screenshots on live screen...");
    let mut durations = Vec::with_capacity(count);
    let mut last_raw = 0;
    let mut last_compressed = 0;
    let mut last_wire = 0;

    for i in 1..=count {
        let t0 = Instant::now();
        let shot = match director.take() {
            Ok(s) => s,
            Err(err) => {
                eprintln!("Failed to capture screenshot #{i}: {err}");
                let _ = director.cut();
                std::process::exit(1);
            }
        };
        let elapsed = t0.elapsed();
        let bytes = shot.raw().len();
        let compressed = shot.compressed_bytes();
        let wire = shot.wire_bytes();
        last_raw = bytes;
        last_compressed = compressed;
        last_wire = wire;

        let secs = elapsed.as_secs_f64();
        let mb_per_sec = if secs > 0.0 { 0.3042 / secs } else { 0.0 };
        let raw_f64 = f64::from(u32::try_from(bytes).unwrap_or(0));
        let wire_f64 = f64::from(u32::try_from(wire).unwrap_or(0));
        let pct = if raw_f64 > 0.0 {
            (wire_f64 / raw_f64) * 100.0
        } else {
            0.0
        };

        println!(
            "Shot #{i}: {bytes} bytes raw -> {compressed} bytes RLE ({wire} b64 chars, {pct:.1}% of raw) in {elapsed:?} ({mb_per_sec:.2} MB/s)"
        );
        durations.push(elapsed);
        std::thread::sleep(Duration::from_millis(50));
    }

    if let Err(err) = director.cut() {
        eprintln!("Warning: clean cut failed: {err}");
    }

    let divisor = u32::try_from(durations.len()).unwrap_or(1);
    let total: Duration = durations.iter().sum();
    let avg = total.checked_div(divisor).unwrap_or(total);
    let min = durations.iter().min().copied().unwrap_or_default();
    let max = durations.iter().max().copied().unwrap_or_default();
    let avg_secs = avg.as_secs_f64();
    let avg_mb_per_sec = if avg_secs > 0.0 {
        0.3042 / avg_secs
    } else {
        0.0
    };

    let raw_f64 = f64::from(u32::try_from(last_raw).unwrap_or(0));
    let wire_f64 = f64::from(u32::try_from(last_wire).unwrap_or(0));
    let ratio = if raw_f64 > 0.0 {
        (wire_f64 / raw_f64) * 100.0
    } else {
        0.0
    };
    let reduction = 100.0 - ratio;

    println!("\n=== SCREENSHOT BENCHMARK RESULTS ===");
    println!("Samples:         {count}");
    println!("Raw size:        {last_raw} bytes");
    println!("Compressed size: {last_compressed} bytes RLE ({last_wire} b64 chars)");
    println!("Wire ratio:      {ratio:.1}% of raw ({reduction:.1}% reduction)");
    println!("Average time:    {avg:?}");
    println!("Min time:        {min:?}");
    println!("Max time:        {max:?}");
    println!("Jitter:          {:?}", max.saturating_sub(min));
    println!("Throughput:      {avg_mb_per_sec:.2} MB/s uncompressed equivalent");
}
