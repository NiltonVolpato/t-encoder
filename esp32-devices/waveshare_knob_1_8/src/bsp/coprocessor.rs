// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Inter-MCU UART communication with the onboard ESP32 co-processor.

use core::sync::atomic::{AtomicBool, Ordering};

use coprocessor::{FrameAccumulator, encode_packet, flatbuffers, parse_response_envelope, proto};
use defmt::{debug, error, info, warn};
use embassy_futures::join::join;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::Timer;
use esp_hal::peripherals::{GPIO38, GPIO48, UART1};
use esp_hal::uart::{Config, Uart};

static IS_LINKED: AtomicBool = AtomicBool::new(false);
static WIFI_CONNECTED: AtomicBool = AtomicBool::new(false);
static IS_PROVISIONING: AtomicBool = AtomicBool::new(false);
static WIFI_SSID: embassy_sync::blocking_mutex::Mutex<
    CriticalSectionRawMutex,
    core::cell::RefCell<heapless::String<32>>,
> = embassy_sync::blocking_mutex::Mutex::new(core::cell::RefCell::new(heapless::String::new()));

/// Returns true if currently connected to Wi-Fi.
pub fn is_wifi_connected() -> bool {
    WIFI_CONNECTED.load(Ordering::Relaxed)
}

/// Returns true if BLE Improv provisioning session is active.
pub fn is_provisioning() -> bool {
    IS_PROVISIONING.load(Ordering::Relaxed)
}

/// Returns the current connected SSID, if any.
pub fn get_wifi_ssid() -> heapless::String<32> {
    WIFI_SSID.lock(|cell| cell.borrow().clone())
}

#[derive(Debug, Clone)]
pub enum CoprocessorCommand {
    StartProvisioning { timeout_seconds: u32 },
    StopProvisioning,
    WifiConnect { ssid: heapless::String<32>, password: heapless::String<64> },
}

static COMMAND_CHANNEL: Channel<CriticalSectionRawMutex, CoprocessorCommand, 4> = Channel::new();

/// Request the co-processor to open an on-demand BLE Improv provisioning session.
pub fn start_provisioning(timeout_seconds: u32) {
    let _ = COMMAND_CHANNEL.try_send(CoprocessorCommand::StartProvisioning { timeout_seconds });
}

/// Request the co-processor to stop any active BLE Improv provisioning session.
pub fn stop_provisioning() {
    let _ = COMMAND_CHANNEL.try_send(CoprocessorCommand::StopProvisioning);
}

/// Request the co-processor to connect to a specific Wi-Fi network.
pub fn connect_wifi(ssid: &str, password: &str) {
    let mut s = heapless::String::new();
    let _ = s.push_str(ssid);
    let mut p = heapless::String::new();
    let _ = p.push_str(password);
    let _ = COMMAND_CHANNEL.try_send(CoprocessorCommand::WifiConnect { ssid: s, password: p });
}

fn handle_coprocessor_response(payload: &[u8], last_uptime: &mut u64) {
    match parse_response_envelope(payload) {
        Ok(env) => match env.message_type() {
            proto::Response::WifiStatus => {
                if let Some(status) = env.message_as_wifi_status() {
                    WIFI_CONNECTED.store(status.connected(), Ordering::Relaxed);
                    let mut s = heapless::String::new();
                    if let Some(ssid) = status.ssid() {
                        let _ = s.push_str(ssid);
                        WIFI_SSID.lock(|cell| *cell.borrow_mut() = s.clone());
                    }
                    theme::update_system_menu_state(|menu| {
                        menu.wifi_connected = status.connected();
                        menu.wifi_ssid = s;
                    });
                    debug!(
                        "[COPROCESSOR] Wi-Fi Status: connected={}, ssid={}, ip={}, rssi={}",
                        status.connected(),
                        status.ssid().unwrap_or(""),
                        status.ip().unwrap_or(""),
                        status.rssi()
                    );
                }
            }
            proto::Response::ProvisioningStatus => {
                if let Some(status) = env.message_as_provisioning_status() {
                    let active = status.state() == proto::ProvisioningState::Active;
                    IS_PROVISIONING.store(active, Ordering::Relaxed);
                    theme::update_system_menu_state(|menu| {
                        menu.is_provisioning = active;
                    });
                    debug!(
                        "[COPROCESSOR] Provisioning Status: {:?}",
                        defmt::Debug2Format(&status.state())
                    );
                }
            }
            proto::Response::Hello => {
                if IS_LINKED.load(Ordering::Relaxed) {
                    warn!(
                        "[COPROCESSOR] Co-processor reboot detected via Hello while linked! Rebooting S3..."
                    );
                    esp_hal::system::software_reset();
                } else {
                    info!("[COPROCESSOR] First Hello received from co-processor, linking session");
                    IS_LINKED.store(true, Ordering::Relaxed);
                }
            }
            proto::Response::Heartbeat => {
                if let Some(hb) = env.message_as_heartbeat() {
                    let uptime = hb.uptime_ms();
                    if IS_LINKED.load(Ordering::Relaxed)
                        && *last_uptime > 0
                        && uptime < *last_uptime
                    {
                        warn!(
                            "[COPROCESSOR] Co-processor uptime dropped ({} < {}), reboot detected! Rebooting S3...",
                            uptime, *last_uptime
                        );
                        esp_hal::system::software_reset();
                    }
                    *last_uptime = uptime;
                    debug!(
                        "[COPROCESSOR] Heartbeat ACK: uptime={}ms, free_heap={}",
                        uptime,
                        hb.heap_free()
                    );
                }
            }
            proto::Response::TimeSync => {
                if let Some(sync) = env.message_as_time_sync() {
                    crate::bsp::rtc::sync_rtc(sync.epoch_seconds(), sync.subsec_micros());
                }
            }
            _ => {
                debug!("[COPROCESSOR] Received response type: {:?}", env.message_type().0);
            }
        },
        Err(e) => {
            warn!("[COPROCESSOR] Failed to parse ResponseEnvelope: {:?}", defmt::Debug2Format(&e));
        }
    }
}

#[embassy_executor::task]
pub async fn coprocessor_task(
    uart: UART1<'static>,
    tx_pin: GPIO38<'static>,
    rx_pin: GPIO48<'static>,
) {
    info!("[COPROCESSOR] Initializing UART1 (TX=GPIO38, RX=GPIO48) at 115200 baud...");

    let uart = match Uart::new(uart, Config::default()) {
        Ok(u) => u.with_tx(tx_pin).with_rx(rx_pin).into_async(),
        Err(e) => {
            error!("[COPROCESSOR] Failed to initialize UART1: {:?}", defmt::Debug2Format(&e));
            return;
        }
    };

    let (mut rx, mut tx) = uart.split();

    let rx_fut = async {
        let mut accumulator = FrameAccumulator::<2048>::new();
        let mut buf = [0u8; 64];
        let mut last_coprocessor_uptime = 0u64;
        loop {
            match rx.read_async(&mut buf).await {
                Ok(n) => {
                    debug!("[COPROCESSOR] UART RX: read {} bytes", n);
                    for &b in &buf[..n] {
                        if let Some(res) = accumulator.push_byte(b) {
                            match res {
                                Ok(payload) => {
                                    debug!(
                                        "[COPROCESSOR] Frame assembled ({} bytes), handling response",
                                        payload.len()
                                    );
                                    handle_coprocessor_response(
                                        payload,
                                        &mut last_coprocessor_uptime,
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        "[COPROCESSOR] Codec error: {:?}",
                                        defmt::Debug2Format(&e)
                                    );
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("[COPROCESSOR] UART RX error: {:?}", defmt::Debug2Format(&e));
                    Timer::after_millis(50).await;
                }
            }
        }
    };

    let tx_fut = async {
        while !IS_LINKED.load(Ordering::Relaxed) {
            let mut builder = flatbuffers::FlatBufferBuilder::new();
            let hello = proto::Hello::create(&mut builder, &proto::HelloArgs {});
            let env = proto::RequestEnvelope::create(
                &mut builder,
                &proto::RequestEnvelopeArgs {
                    message_type: proto::Request::Hello,
                    message: Some(hello.as_union_value()),
                },
            );
            builder.finish_size_prefixed(env, None);
            let packet = encode_packet(builder.finished_data());
            debug!("[COPROCESSOR] TX Hello request sent ({} bytes)", packet.len());
            if let Err(e) = tx.write_async(&packet).await {
                error!("[COPROCESSOR] UART TX error: {:?}", defmt::Debug2Format(&e));
            }
            let _ = tx.flush_async().await;
            Timer::after_millis(500).await;
        }

        loop {
            match select(Timer::after_secs(10), COMMAND_CHANNEL.receive()).await {
                Either::First(_) => {
                    let mut builder = flatbuffers::FlatBufferBuilder::new();
                    let hb = proto::Heartbeat::create(
                        &mut builder,
                        &proto::HeartbeatArgs {
                            uptime_ms: esp_hal::time::Instant::now()
                                .duration_since_epoch()
                                .as_millis(),
                            heap_free: 0,
                        },
                    );
                    let env = proto::RequestEnvelope::create(
                        &mut builder,
                        &proto::RequestEnvelopeArgs {
                            message_type: proto::Request::Heartbeat,
                            message: Some(hb.as_union_value()),
                        },
                    );
                    builder.finish_size_prefixed(env, None);
                    let packet = encode_packet(builder.finished_data());
                    debug!("[COPROCESSOR] TX Heartbeat request sent ({} bytes)", packet.len());
                    if let Err(e) = tx.write_async(&packet).await {
                        error!("[COPROCESSOR] UART TX error: {:?}", defmt::Debug2Format(&e));
                    }
                    let _ = tx.flush_async().await;
                }
                Either::Second(cmd) => {
                    let mut builder = flatbuffers::FlatBufferBuilder::new();
                    let env = match cmd {
                        CoprocessorCommand::StartProvisioning { timeout_seconds } => {
                            let sp = proto::StartProvisioning::create(
                                &mut builder,
                                &proto::StartProvisioningArgs { timeout_seconds },
                            );
                            proto::RequestEnvelope::create(
                                &mut builder,
                                &proto::RequestEnvelopeArgs {
                                    message_type: proto::Request::StartProvisioning,
                                    message: Some(sp.as_union_value()),
                                },
                            )
                        }
                        CoprocessorCommand::StopProvisioning => {
                            let sp = proto::StopProvisioning::create(
                                &mut builder,
                                &proto::StopProvisioningArgs {},
                            );
                            proto::RequestEnvelope::create(
                                &mut builder,
                                &proto::RequestEnvelopeArgs {
                                    message_type: proto::Request::StopProvisioning,
                                    message: Some(sp.as_union_value()),
                                },
                            )
                        }
                        CoprocessorCommand::WifiConnect { ssid, password } => {
                            let ssid_off = builder.create_string(&ssid);
                            let pass_off = builder.create_string(&password);
                            let req = proto::WifiConnectRequest::create(
                                &mut builder,
                                &proto::WifiConnectRequestArgs {
                                    ssid: Some(ssid_off),
                                    password: Some(pass_off),
                                },
                            );
                            proto::RequestEnvelope::create(
                                &mut builder,
                                &proto::RequestEnvelopeArgs {
                                    message_type: proto::Request::WifiConnectRequest,
                                    message: Some(req.as_union_value()),
                                },
                            )
                        }
                    };
                    builder.finish_size_prefixed(env, None);
                    let packet = encode_packet(builder.finished_data());
                    debug!("[COPROCESSOR] TX command sent ({} bytes)", packet.len());
                    if let Err(e) = tx.write_async(&packet).await {
                        error!("[COPROCESSOR] UART TX error: {:?}", defmt::Debug2Format(&e));
                    }
                    let _ = tx.flush_async().await;
                }
            }
        }
    };

    join(rx_fut, tx_fut).await;
}
