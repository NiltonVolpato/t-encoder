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

#[inline]
fn encode_triplet(in_3: [u8; 3]) -> [u8; 4] {
    let n = ((u32::from(in_3[0])) << 16) | ((u32::from(in_3[1])) << 8) | (u32::from(in_3[2]));
    let idx0 = usize::try_from((n >> 18) & 0x3F).unwrap_or(0);
    let idx1 = usize::try_from((n >> 12) & 0x3F).unwrap_or(0);
    let idx2 = usize::try_from((n >> 6) & 0x3F).unwrap_or(0);
    let idx3 = usize::try_from(n & 0x3F).unwrap_or(0);
    [
        BASE64_TABLE.get(idx0).copied().unwrap_or(b'A'),
        BASE64_TABLE.get(idx1).copied().unwrap_or(b'A'),
        BASE64_TABLE.get(idx2).copied().unwrap_or(b'A'),
        BASE64_TABLE.get(idx3).copied().unwrap_or(b'A'),
    ]
}

struct Base64StreamWriter<'a> {
    tx: &'a mut UsbSerialJtagTx<'static, esp_hal::Async>,
    buf: [u8; 48],
    buf_len: usize,
}

impl<'a> Base64StreamWriter<'a> {
    fn new(tx: &'a mut UsbSerialJtagTx<'static, esp_hal::Async>) -> Self {
        Self {
            tx,
            buf: [0u8; 48],
            buf_len: 0,
        }
    }

    #[inline]
    fn push_byte(&mut self, b: u8) {
        if let Some(slot) = self.buf.get_mut(self.buf_len) {
            *slot = b;
        }
        self.buf_len = self.buf_len.saturating_add(1);
        if self.buf_len == 48 {
            self.flush_48();
        }
    }

    #[inline]
    fn push_slice(&mut self, slice: &[u8]) {
        for &b in slice {
            self.push_byte(b);
        }
    }

    fn flush_48(&mut self) {
        use embedded_io::Write;
        let mut out = [0u8; 64];
        for i in 0usize..16 {
            let in_start = i.saturating_mul(3);
            let in_3 = [
                self.buf.get(in_start).copied().unwrap_or(0),
                self.buf
                    .get(in_start.saturating_add(1))
                    .copied()
                    .unwrap_or(0),
                self.buf
                    .get(in_start.saturating_add(2))
                    .copied()
                    .unwrap_or(0),
            ];
            let out_4 = encode_triplet(in_3);
            let out_start = i.saturating_mul(4);
            let out_end = out_start.saturating_add(4);
            if let Some(dst) = out.get_mut(out_start..out_end) {
                dst.copy_from_slice(&out_4);
            }
        }
        let _ = self.tx.write_all(&out);
        self.buf_len = 0;
    }

    fn finish(&mut self) {
        use embedded_io::Write;
        let full_triplets = self.buf_len / 3;
        let mut out = [0u8; 64];
        for i in 0..full_triplets {
            let in_start = i.saturating_mul(3);
            let in_3 = [
                self.buf.get(in_start).copied().unwrap_or(0),
                self.buf
                    .get(in_start.saturating_add(1))
                    .copied()
                    .unwrap_or(0),
                self.buf
                    .get(in_start.saturating_add(2))
                    .copied()
                    .unwrap_or(0),
            ];
            let out_4 = encode_triplet(in_3);
            let out_start = i.saturating_mul(4);
            let out_end = out_start.saturating_add(4);
            if let Some(dst) = out.get_mut(out_start..out_end) {
                dst.copy_from_slice(&out_4);
            }
        }
        let full_bytes = full_triplets.saturating_mul(4);
        if full_bytes > 0
            && let Some(slice) = out.get(..full_bytes)
        {
            let _ = self.tx.write_all(slice);
        }

        let remainder = self.buf_len % 3;
        let rem_start = full_triplets.saturating_mul(3);
        if remainder == 1 {
            let b0 = self.buf.get(rem_start).copied().unwrap_or(0);
            let n = (u32::from(b0)) << 16;
            let idx0 = usize::try_from((n >> 18) & 0x3F).unwrap_or(0);
            let idx1 = usize::try_from((n >> 12) & 0x3F).unwrap_or(0);
            let out_4 = [
                BASE64_TABLE.get(idx0).copied().unwrap_or(b'A'),
                BASE64_TABLE.get(idx1).copied().unwrap_or(b'A'),
                b'=',
                b'=',
            ];
            let _ = self.tx.write_all(&out_4);
        } else if remainder == 2 {
            let b0 = self.buf.get(rem_start).copied().unwrap_or(0);
            let b1 = self
                .buf
                .get(rem_start.saturating_add(1))
                .copied()
                .unwrap_or(0);
            let n = ((u32::from(b0)) << 16) | ((u32::from(b1)) << 8);
            let idx0 = usize::try_from((n >> 18) & 0x3F).unwrap_or(0);
            let idx1 = usize::try_from((n >> 12) & 0x3F).unwrap_or(0);
            let idx2 = usize::try_from((n >> 6) & 0x3F).unwrap_or(0);
            let out_4 = [
                BASE64_TABLE.get(idx0).copied().unwrap_or(b'A'),
                BASE64_TABLE.get(idx1).copied().unwrap_or(b'A'),
                BASE64_TABLE.get(idx2).copied().unwrap_or(b'A'),
                b'=',
            ];
            let _ = self.tx.write_all(&out_4);
        }
        self.buf_len = 0;
    }
}

#[inline]
fn get_pixel(fb: &[u8], idx: usize) -> [u8; 2] {
    let offset = idx.saturating_mul(2);
    [
        fb.get(offset).copied().unwrap_or(0),
        fb.get(offset.saturating_add(1)).copied().unwrap_or(0),
    ]
}

/// Streams framebuffer bytes to USB TX as TGA-RLE compressed base64 within a single JSON line.
fn stream_screenshot_base64(tx: &mut UsbSerialJtagTx<'static, esp_hal::Async>, fb: &[u8]) {
    use embedded_io::Write;
    let ts_us = embassy_time::Instant::now().as_micros();
    let _ = tx.write_all(b"{\"type\":\"screenshot\",\"width\":390,\"height\":390,\"ts_us\":");
    let mut ts_buf = heapless::String::<32>::new();
    let _ = core::fmt::write(&mut ts_buf, format_args!("{ts_us}"));
    let _ = tx.write_all(ts_buf.as_bytes());
    let _ = tx.write_all(b",\"data\":\"");

    let total_pixels = fb.len() / 2;
    let mut writer = Base64StreamWriter::new(tx);
    let mut i = 0;

    while i < total_pixels {
        let px = get_pixel(fb, i);
        if i.saturating_add(1) < total_pixels && get_pixel(fb, i.saturating_add(1)) == px {
            let mut run_len = 2;
            while i.saturating_add(run_len) < total_pixels
                && run_len < 128
                && get_pixel(fb, i.saturating_add(run_len)) == px
            {
                run_len = run_len.saturating_add(1);
            }
            let header = 0x80 | u8::try_from(run_len.saturating_sub(1)).unwrap_or(0);
            writer.push_byte(header);
            writer.push_slice(&px);
            i = i.saturating_add(run_len);
        } else {
            let mut lit_len = 1;
            while i.saturating_add(lit_len) < total_pixels && lit_len < 128 {
                let next_idx = i.saturating_add(lit_len);
                let after_idx = next_idx.saturating_add(1);
                if after_idx < total_pixels && get_pixel(fb, next_idx) == get_pixel(fb, after_idx) {
                    break;
                }
                lit_len = lit_len.saturating_add(1);
            }
            let header = u8::try_from(lit_len.saturating_sub(1)).unwrap_or(0);
            writer.push_byte(header);
            let start_byte = i.saturating_mul(2);
            let end_byte = start_byte.saturating_add(lit_len.saturating_mul(2));
            if let Some(slice) = fb.get(start_byte..end_byte) {
                writer.push_slice(slice);
            }
            i = i.saturating_add(lit_len);
        }
    }
    writer.finish();

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
                        ts_us: embassy_time::Instant::now().as_micros(),
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
                ts_us: embassy_time::Instant::now().as_micros(),
                error: None,
            }));
        }
        Ok(IncomingCommand::Screenshot) => {
            TX_CHANNEL.send(TxMessage::Screenshot).await;
        }
        Ok(IncomingCommand::Reset) => {
            TX_CHANNEL
                .send(TxMessage::Response(CommandResponse {
                    ok: true,
                    ts_us: embassy_time::Instant::now().as_micros(),
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
                ts_us: embassy_time::Instant::now().as_micros(),
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
