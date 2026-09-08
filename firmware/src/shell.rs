//! Serial shell task over native USB-Serial/JTAG using `embedded-cli`.

use core::convert::Infallible;
use embassy_time::Instant;
use embedded_cli::Command;
use embedded_cli::cli::{CliBuilder, CliHandle};
use embedded_io::{ErrorType, Write};
use enc_state::{AppState, ConnState};
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
fn on_stats(
    cli: &mut CliHandle<'_, SerialWriter<'_>, Infallible>,
    app_state: &AppState,
) -> Result<(), Infallible> {
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

    let conn_str = match app_state.conn() {
        ConnState::Disconnected => "disconnected",
        ConnState::Connecting => "connecting",
        ConnState::Connected => "connected",
    };

    if let Some([a, b, c, d]) = app_state.ip() {
        uwriteln!(cli.writer(), "wifi: {} ({}.{}.{}.{})", conn_str, a, b, c, d)?;
    } else {
        uwriteln!(cli.writer(), "wifi: {}", conn_str)?;
    }

    Ok(())
}

/// Static buffer sizes for command line and history.
const COMMAND_BUFFER_SIZE: usize = 64;
const HISTORY_BUFFER_SIZE: usize = 128;

/// Asynchronous Embassy task driving the serial CLI.
#[embassy_executor::task]
pub async fn task(
    mut rx: UsbSerialJtagRx<'static, esp_hal::Async>,
    mut tx: UsbSerialJtagTx<'static, esp_hal::Async>,
    app_state: &'static AppState,
) {
    let mut command_buffer = [0u8; COMMAND_BUFFER_SIZE];
    let mut history_buffer = [0u8; HISTORY_BUFFER_SIZE];
    let writer = SerialWriter(&mut tx);

    let mut cli = match CliBuilder::default()
        .prompt("> ")
        .writer(writer)
        .command_buffer(&mut command_buffer[..])
        .history_buffer(&mut history_buffer[..])
        .build()
    {
        Ok(cli) => cli,
        Err(_) => return,
    };

    let mut read_buf = [0u8; 16];
    loop {
        match embedded_io_async::Read::read(&mut rx, &mut read_buf).await {
            Ok(0) => {}
            Ok(count) => {
                for &b in &read_buf[..count] {
                    let _ = cli.process_byte::<ShellCommand<'_>, _>(
                        b,
                        &mut ShellCommand::processor(|cli, command| match command {
                            ShellCommand::Log { level } => on_log(cli, level),
                            ShellCommand::Stats => on_stats(cli, app_state),
                        }),
                    );
                }
            }
            Err(_) => {}
        }
    }
}
