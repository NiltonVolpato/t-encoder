// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#pragma once

#include <cstdint>
#include "hal/uart_types.h"
#include "coprocessor_generated.h"

// Hardware pinout for ESP32-U4WDH inter-MCU UART to ESP32-S3
constexpr uart_port_t COPROCESSOR_UART_PORT = UART_NUM_1;
constexpr int COPROCESSOR_UART_TX_PIN = 18;
constexpr int COPROCESSOR_UART_RX_PIN = 23;
constexpr int COPROCESSOR_UART_BAUD = 115200;

using wifi_connect_request_cb_t = void (*)(const char *ssid, const char *password);
using start_provisioning_cb_t = void (*)(uint32_t timeout_seconds);
using stop_provisioning_cb_t = void (*)();

void uart_protocol_init(wifi_connect_request_cb_t wifi_cb,
                        start_provisioning_cb_t start_prov_cb,
                        stop_provisioning_cb_t stop_prov_cb);
void uart_send_wifi_status(bool connected, const char *ssid, const char *ip_addr, int16_t rssi);
void uart_send_heartbeat_response(uint64_t uptime_ms, uint32_t heap_free);
void uart_send_provisioning_status(CoprocessorProto::ProvisioningState state);
