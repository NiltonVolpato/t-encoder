// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

//! Inter-MCU UART communication with the onboard ESP32 co-processor.

use coprocessor::{FrameAccumulator, encode_packet, flatbuffers, parse_response_envelope, proto};
use defmt::{error, info, warn};
use embassy_futures::join::join;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::Timer;
use esp_hal::peripherals::{GPIO38, GPIO48, UART1};
use esp_hal::uart::{Config, Uart};

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

fn handle_coprocessor_response(payload: &[u8]) {
    match parse_response_envelope(payload) {
        Ok(env) => match env.message_type() {
            proto::Response::WifiStatus => {
                if let Some(status) = env.message_as_wifi_status() {
                    info!(
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
                    info!(
                        "[COPROCESSOR] Provisioning Status: {:?}",
                        defmt::Debug2Format(&status.state())
                    );
                }
            }
            proto::Response::Heartbeat => {
                if let Some(hb) = env.message_as_heartbeat() {
                    info!(
                        "[COPROCESSOR] Heartbeat ACK: uptime={}ms, free_heap={}",
                        hb.uptime_ms(),
                        hb.heap_free()
                    );
                }
            }
            _ => {
                info!("[COPROCESSOR] Received response type: {:?}", env.message_type().0);
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
        loop {
            match rx.read_async(&mut buf).await {
                Ok(n) => {
                    for &b in &buf[..n] {
                        if let Some(res) = accumulator.push_byte(b) {
                            match res {
                                Ok(payload) => handle_coprocessor_response(payload),
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
                    if let Err(e) = tx.write_async(&packet).await {
                        error!("[COPROCESSOR] UART TX error: {:?}", defmt::Debug2Format(&e));
                    }
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
                    if let Err(e) = tx.write_async(&packet).await {
                        error!("[COPROCESSOR] UART TX error: {:?}", defmt::Debug2Format(&e));
                    }
                }
            }
        }
    };

    join(rx_fut, tx_fut).await;
}
