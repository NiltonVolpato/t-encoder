//! Serial shell task over native USB-Serial/JTAG using `embedded-cli`.

use core::convert::Infallible;
use embassy_time::Instant;
use embedded_cli::Command;
use embedded_cli::cli::{CliBuilder, CliHandle};
use embedded_io::{ErrorType, Write};
use esp_hal::usb_serial_jtag::{UsbSerialJtagRx, UsbSerialJtagTx};
use ufmt::uwriteln;

/// Commands accepted by the interactive shell.
#[derive(Debug, Command)]
enum ShellCommand<'a> {
    /// Get or set log level (off, error, warn, info, debug, trace)
    Log {
        /// New log level
        level: Option<&'a str>,
    },
    /// Show system uptime, heap memory usage, and Wi-Fi state
    Stats,
    /// Capture and stream raw 304,200 byte RGB565 framebuffer
    Screenshot,
    /// Rotate dial by delta detents (+1 CW, -1 CCW)
    Rotate {
        /// Detent count
        delta: i32,
    },
    /// Dial button short press
    Press,
    /// Dial button long press
    LongPress,
    /// Tap panel at coordinates (x, y)
    Tap {
        /// X coordinate (0..390)
        x: i32,
        /// Y coordinate (0..390)
        y: i32,
    },
    /// Swipe panel in direction (left, right, up, down)
    Swipe {
        /// Direction
        direction: &'a str,
    },
    /// Reboot the device via software reset
    Reset,
}

/// Output writer wrapping [`UsbSerialJtagTx`].
pub struct SerialWriter<'a>(pub &'a mut UsbSerialJtagTx<'static, esp_hal::Async>);

impl ErrorType for SerialWriter<'_> {
    type Error = Infallible;
}

impl Write for SerialWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let n = embedded_io::Write::write(self.0, buf).unwrap_or(0);
        Ok(n)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        let _ = embedded_io::Write::flush(self.0);
        Ok(())
    }
}

/// Handles the `log` shell command.
fn on_log(
    cli: &mut CliHandle<'_, SerialWriter<'_>, Infallible>,
    level: Option<&str>,
) -> Result<(), Infallible> {
    match level {
        None => {
            let current = match log::max_level() {
                log::LevelFilter::Off => "off",
                log::LevelFilter::Error => "error",
                log::LevelFilter::Warn => "warn",
                log::LevelFilter::Info => "info",
                log::LevelFilter::Debug => "debug",
                log::LevelFilter::Trace => "trace",
            };
            uwriteln!(cli.writer(), "current log level: {}", current)?;
        }
        Some(lvl) => {
            let filter = match lvl {
                "off" | "OFF" => Some(log::LevelFilter::Off),
                "error" | "ERROR" => Some(log::LevelFilter::Error),
                "warn" | "WARN" => Some(log::LevelFilter::Warn),
                "info" | "INFO" => Some(log::LevelFilter::Info),
                "debug" | "DEBUG" => Some(log::LevelFilter::Debug),
                "trace" | "TRACE" => Some(log::LevelFilter::Trace),
                _ => None,
            };
            match filter {
                Some(f) => {
                    log::set_max_level(f);
                    uwriteln!(cli.writer(), "log level set to: {}", lvl)?;
                }
                None => {
                    cli.writer().write_str(
                        "unknown log level. Valid: off, error, warn, info, debug, trace\r\n",
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// Handles the `stats` shell command.
fn on_stats(cli: &mut CliHandle<'_, SerialWriter<'_>, Infallible>) -> Result<(), Infallible> {
    let uptime = Instant::now().as_secs();
    uwriteln!(cli.writer(), "uptime: {}s", uptime)?;

    let internal_free = esp_alloc::HEAP.free();
    let internal_used = esp_alloc::HEAP.used();
    uwriteln!(
        cli.writer(),
        "heap internal: free={} used={}",
        internal_free,
        internal_used
    )?;

    let psram_free = crate::heap::PSRAM_HEAP.free();
    let psram_used = crate::heap::PSRAM_HEAP.used();
    uwriteln!(
        cli.writer(),
        "heap psram: free={} used={}",
        psram_free,
        psram_used
    )?;

    if let Some((ip, connected)) = crate::radio::wifi_info() {
        let [a, b, c, d] = ip;
        let conn_str = if connected { "connected" } else { "connecting" };
        uwriteln!(cli.writer(), "wifi: {} ({}.{}.{}.{})", conn_str, a, b, c, d)?;
    } else {
        uwriteln!(cli.writer(), "wifi: not connected")?;
    }

    let ble_str = if crate::radio::ble_linked() {
        "linked"
    } else {
        "idle"
    };
    uwriteln!(cli.writer(), "ble: {}", ble_str)?;

    Ok(())
}

static TX_PTR: core::sync::atomic::AtomicPtr<UsbSerialJtagTx<'static, esp_hal::Async>> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

fn write_raw_bytes(tx: &mut UsbSerialJtagTx<'static, esp_hal::Async>, mut buf: &[u8]) {
    while !buf.is_empty() {
        match embedded_io::Write::write(tx, buf) {
            Ok(0) => {}
            Ok(n) => {
                buf = buf.get(n..).unwrap_or(&[]);
            }
            Err(_) => break,
        }
    }
    let _ = embedded_io::Write::flush(tx);
}

/// Handles the `screenshot` shell command.
fn on_screenshot(cli: &mut CliHandle<'_, SerialWriter<'_>, Infallible>) -> Result<(), Infallible> {
    let Some(fb) = crate::heap::framebuffer_slice() else {
        cli.writer()
            .write_str("error: framebuffer unavailable\r\n")?;
        return Ok(());
    };

    let prev_filter = log::max_level();
    log::set_max_level(log::LevelFilter::Off);

    // Framebuffer header: SCREENSHOT <width> <height> <bytes>
    uwriteln!(cli.writer(), "SCREENSHOT 390 390 {}", fb.len())?;

    let ptr = TX_PTR.load(core::sync::atomic::Ordering::Acquire);
    if let Some(tx) = unsafe { ptr.as_mut() } {
        write_raw_bytes(tx, fb);
    }

    log::set_max_level(prev_filter);
    cli.writer().write_str("\r\n")?;
    Ok(())
}

fn on_rotate(
    cli: &mut CliHandle<'_, SerialWriter<'_>, Infallible>,
    delta: i32,
) -> Result<(), Infallible> {
    crate::event::send(crate::event::Event::Rotate(delta));
    uwriteln!(cli.writer(), "ok: rotate {}", delta)?;
    Ok(())
}

fn on_press(cli: &mut CliHandle<'_, SerialWriter<'_>, Infallible>) -> Result<(), Infallible> {
    crate::event::send(crate::event::Event::ShortPress);
    cli.writer().write_str("ok: press\r\n")?;
    Ok(())
}

fn on_long_press(cli: &mut CliHandle<'_, SerialWriter<'_>, Infallible>) -> Result<(), Infallible> {
    crate::event::send(crate::event::Event::LongPress);
    cli.writer().write_str("ok: long-press\r\n")?;
    Ok(())
}

fn on_tap(
    cli: &mut CliHandle<'_, SerialWriter<'_>, Infallible>,
    x: i32,
    y: i32,
) -> Result<(), Infallible> {
    crate::event::send(crate::event::Event::Gesture(launcher::Gesture::Tap {
        x,
        y,
    }));
    uwriteln!(cli.writer(), "ok: tap {} {}", x, y)?;
    Ok(())
}

fn on_swipe(
    cli: &mut CliHandle<'_, SerialWriter<'_>, Infallible>,
    direction: &str,
) -> Result<(), Infallible> {
    let gesture = match direction {
        "left" | "LEFT" => Some(launcher::Gesture::SwipeLeft),
        "right" | "RIGHT" => Some(launcher::Gesture::SwipeRight),
        "up" | "UP" => Some(launcher::Gesture::SwipeUp),
        "down" | "DOWN" => Some(launcher::Gesture::SwipeDown),
        _ => None,
    };
    match gesture {
        Some(g) => {
            crate::event::send(crate::event::Event::Gesture(g));
            uwriteln!(cli.writer(), "ok: swipe {}", direction)?;
        }
        None => {
            cli.writer()
                .write_str("error: unknown direction. Valid: left, right, up, down\r\n")?;
        }
    }
    Ok(())
}

fn on_reset(cli: &mut CliHandle<'_, SerialWriter<'_>, Infallible>) -> Result<(), Infallible> {
    cli.writer().write_str("ok: rebooting...\r\n")?;
    let ptr = TX_PTR.load(core::sync::atomic::Ordering::Acquire);
    if let Some(tx) = unsafe { ptr.as_mut() } {
        let _ = embedded_io::Write::flush(tx);
    }
    esp_hal::delay::Delay::new().delay_millis(50);
    esp_hal::system::software_reset();
}

/// Static buffer sizes for command line and history.
const COMMAND_BUFFER_SIZE: usize = 64;
const HISTORY_BUFFER_SIZE: usize = 128;

/// Waits for the host to send an initial byte before bringing up the CLI.
///
/// `CliBuilder::build()` immediately flushes the prompt to TX; if no USB host
/// terminal is attached, synchronous USB-Serial-JTAG writes block indefinitely at boot.
/// Waiting for an initial keystroke ensures a terminal is connected before writing to TX.
async fn wait_for_connection(rx: &mut UsbSerialJtagRx<'static, esp_hal::Async>) {
    log::info!("Console ready. Press Enter to connect");
    let mut byte = [0u8; 1];
    let _ = embedded_io_async::Read::read(rx, &mut byte).await;
}

/// Asynchronous Embassy task driving the serial CLI.
#[embassy_executor::task]
pub async fn task(
    mut rx: UsbSerialJtagRx<'static, esp_hal::Async>,
    mut tx: UsbSerialJtagTx<'static, esp_hal::Async>,
) {
    TX_PTR.store(&raw mut tx, core::sync::atomic::Ordering::Release);
    let mut command_buffer = [0u8; COMMAND_BUFFER_SIZE];
    let mut history_buffer = [0u8; HISTORY_BUFFER_SIZE];
    let writer = SerialWriter(&mut tx);

    wait_for_connection(&mut rx).await;

    let Ok(mut cli) = CliBuilder::default()
        .prompt("> ")
        .writer(writer)
        .command_buffer(&mut command_buffer[..])
        .history_buffer(&mut history_buffer[..])
        .build();

    let mut read_buf = [0u8; 16];
    loop {
        match embedded_io_async::Read::read(&mut rx, &mut read_buf).await {
            Ok(0) | Err(_) => {}
            Ok(count) => {
                if let Some(bytes) = read_buf.get(..count) {
                    for &b in bytes {
                        let _ = cli.process_byte::<ShellCommand<'_>, _>(
                            b,
                            &mut ShellCommand::processor(|cli, command| match command {
                                ShellCommand::Log { level } => on_log(cli, level),
                                ShellCommand::Stats => on_stats(cli),
                                ShellCommand::Screenshot => on_screenshot(cli),
                                ShellCommand::Rotate { delta } => on_rotate(cli, delta),
                                ShellCommand::Press => on_press(cli),
                                ShellCommand::LongPress => on_long_press(cli),
                                ShellCommand::Tap { x, y } => on_tap(cli, x, y),
                                ShellCommand::Swipe { direction } => on_swipe(cli, direction),
                                ShellCommand::Reset => on_reset(cli),
                            }),
                        );
                    }
                }
            }
        }
    }
}
