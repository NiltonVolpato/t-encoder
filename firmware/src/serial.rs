//! Serial communication task over native USB-Serial/JTAG using NDJSON.

use alloc::string::ToString;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use esp_hal::usb_serial_jtag::{UsbSerialJtagRx, UsbSerialJtagTx};
use protocol::{CommandResponse, DeviceEvent, DeviceMessage, IncomingCommand, LogRecord};

/// Outgoing message queued for transmission over USB.
pub enum TxMessage {
    /// Diagnostic log record.
    Log(LogRecord),
    /// Domain or lifecycle event.
    Event(DeviceEvent),
    /// Response to a command.
    Response(CommandResponse),
    /// Framebuffer capture streaming request.
    Screenshot,
}

/// Channel queuing outgoing messages for the serial transmitter task.
pub static TX_CHANNEL: Channel<CriticalSectionRawMutex, TxMessage, 32> = Channel::new();

/// Called by the custom logger subscriber to queue a log record.
pub fn log_record(record: &log::Record) {
    let mut msg_buf = heapless::String::<256>::new();
    let _ = core::fmt::write(&mut msg_buf, *record.args());

    let rec = LogRecord {
        ts_us: embassy_time::Instant::now().as_micros(),
        level: record.level().as_str().to_string(),
        target: record.target().to_string(),
        msg: msg_buf.as_str().to_string(),
    };
    let _ = TX_CHANNEL.try_send(TxMessage::Log(rec));
}

/// Broadcasts a device event to the host over NDJSON.
pub fn broadcast_event(event: DeviceEvent) {
    let _ = TX_CHANNEL.try_send(TxMessage::Event(event));
}

const BASE64_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

#[inline(always)]
fn encode_triplet(in_3: &[u8; 3], out_4: &mut [u8; 4]) {
    let n = ((u32::from(in_3[0])) << 16) | ((u32::from(in_3[1])) << 8) | (u32::from(in_3[2]));
    out_4[0] = BASE64_TABLE[((n >> 18) & 0x3F) as usize];
    out_4[1] = BASE64_TABLE[((n >> 12) & 0x3F) as usize];
    out_4[2] = BASE64_TABLE[((n >> 6) & 0x3F) as usize];
    out_4[3] = BASE64_TABLE[(n & 0x3F) as usize];
}

/// Streams framebuffer bytes directly to USB TX as base64 within a single JSON line.
fn stream_screenshot_base64(tx: &mut UsbSerialJtagTx<'static, esp_hal::Async>, fb: &[u8]) {
    use embedded_io::Write;
    let _ = tx.write_all(b"{\"type\":\"screenshot\",\"width\":390,\"height\":390,\"data\":\"");

    // 304,200 is exactly divisible by 30 (10,140 chunks of 30 bytes -> 40 chars)
    let mut chunk_out = [0u8; 40];
    for chunk_30 in fb.chunks_exact(30) {
        for (i, in_3) in chunk_30.chunks_exact(3).enumerate() {
            if let Ok(triplet) = in_3.try_into() {
                let mut out_4 = [0u8; 4];
                encode_triplet(triplet, &mut out_4);
                let start = i.saturating_mul(4);
                let end = start.saturating_add(4);
                if let Some(dst) = chunk_out.get_mut(start..end) {
                    dst.copy_from_slice(&out_4);
                }
            }
        }
        let _ = tx.write_all(&chunk_out);
    }

    let _ = tx.write_all(b"\"}\n");
    let _ = tx.flush();
}

/// Asynchronous task draining the TX queue and writing NDJSON lines to USB.
#[embassy_executor::task]
pub async fn tx_task(mut tx: UsbSerialJtagTx<'static, esp_hal::Async>) {
    use embedded_io_async::Write;
    let mut json_buf = alloc::string::String::new();

    loop {
        let msg = TX_CHANNEL.receive().await;
        match msg {
            TxMessage::Screenshot => {
                if let Some(fb) = crate::heap::framebuffer_slice() {
                    stream_screenshot_base64(&mut tx, fb);
                } else {
                    let err = DeviceMessage::Response(CommandResponse {
                        ok: false,
                        error: Some("framebuffer unavailable".to_string()),
                    });
                    json_buf.clear();
                    if let Ok(s) = serde_json::to_string(&err) {
                        json_buf.push_str(&s);
                        json_buf.push('\n');
                        let _ = tx.write_all(json_buf.as_bytes()).await;
                        let _ = tx.flush().await;
                    }
                }
            }
            TxMessage::Log(rec) => {
                let dev_msg = DeviceMessage::Log(rec);
                json_buf.clear();
                if let Ok(s) = serde_json::to_string(&dev_msg) {
                    json_buf.push_str(&s);
                    json_buf.push('\n');
                    let _ = tx.write_all(json_buf.as_bytes()).await;
                    let _ = tx.flush().await;
                }
            }
            TxMessage::Event(ev) => {
                let dev_msg = DeviceMessage::Event(ev);
                json_buf.clear();
                if let Ok(s) = serde_json::to_string(&dev_msg) {
                    json_buf.push_str(&s);
                    json_buf.push('\n');
                    let _ = tx.write_all(json_buf.as_bytes()).await;
                    let _ = tx.flush().await;
                }
            }
            TxMessage::Response(resp) => {
                let dev_msg = DeviceMessage::Response(resp);
                json_buf.clear();
                if let Ok(s) = serde_json::to_string(&dev_msg) {
                    json_buf.push_str(&s);
                    json_buf.push('\n');
                    let _ = tx.write_all(json_buf.as_bytes()).await;
                    let _ = tx.flush().await;
                }
            }
        }
    }
}

async fn handle_command(line: &str) {
    match serde_json::from_str::<IncomingCommand>(line) {
        Ok(IncomingCommand::Cue { cue }) => {
            match cue {
                protocol::CueCommand::Rotate { delta } => {
                    crate::event::send(crate::event::Event::Rotate(delta));
                }
                protocol::CueCommand::Press => {
                    crate::event::send(crate::event::Event::ShortPress);
                }
                protocol::CueCommand::LongPress => {
                    crate::event::send(crate::event::Event::LongPress);
                }
                protocol::CueCommand::Tap { x, y } => {
                    crate::event::send(crate::event::Event::Gesture(launcher::Gesture::Tap {
                        x,
                        y,
                    }));
                }
                protocol::CueCommand::Swipe { direction } => {
                    let gesture = match direction {
                        protocol::SwipeDirection::Left => launcher::Gesture::SwipeLeft,
                        protocol::SwipeDirection::Right => launcher::Gesture::SwipeRight,
                        protocol::SwipeDirection::Up => launcher::Gesture::SwipeUp,
                        protocol::SwipeDirection::Down => launcher::Gesture::SwipeDown,
                    };
                    crate::event::send(crate::event::Event::Gesture(gesture));
                }
            }
            let _ = TX_CHANNEL.try_send(TxMessage::Response(CommandResponse {
                ok: true,
                error: None,
            }));
        }
        Ok(IncomingCommand::Screenshot) => {
            let _ = TX_CHANNEL.send(TxMessage::Screenshot).await;
        }
        Ok(IncomingCommand::Reset) => {
            let _ = TX_CHANNEL
                .send(TxMessage::Response(CommandResponse {
                    ok: true,
                    error: None,
                }))
                .await;
            embassy_time::Timer::after(embassy_time::Duration::from_millis(50)).await;
            esp_hal::system::software_reset();
        }
        Err(e) => {
            let mut err_msg = alloc::string::String::new();
            let _ = core::fmt::write(&mut err_msg, format_args!("malformed command: {e}"));
            let _ = TX_CHANNEL.try_send(TxMessage::Response(CommandResponse {
                ok: false,
                error: Some(err_msg),
            }));
        }
    }
}

/// Asynchronous task reading NDJSON command lines from USB RX.
#[embassy_executor::task]
pub async fn rx_task(mut rx: UsbSerialJtagRx<'static, esp_hal::Async>) {
    use embedded_io_async::Read;
    let mut line_buf = heapless::Vec::<u8, 256>::new();
    let mut byte = [0u8; 1];

    loop {
        match rx.read(&mut byte).await {
            Ok(0) | Err(_) => {
                embassy_time::Timer::after(embassy_time::Duration::from_millis(10)).await;
            }
            Ok(_) => {
                let b = byte[0];
                if b == b'\n' || b == b'\r' {
                    if !line_buf.is_empty() {
                        if let Ok(line_str) = core::str::from_utf8(&line_buf) {
                            handle_command(line_str.trim()).await;
                        }
                        line_buf.clear();
                    }
                } else if line_buf.push(b).is_err() {
                    line_buf.clear();
                }
            }
        }
    }
}
