// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#pragma once

#include <cstdint>
#include <string_view>

#include "coprocessor_generated.h"
#include "hal/uart_types.h"

namespace coprocessor {

// Hardware pinout for ESP32-U4WDH inter-MCU UART to ESP32-S3
constexpr uart_port_t UART_PORT = UART_NUM_1;
constexpr int UART_TX_PIN = 18;
constexpr int UART_RX_PIN = 23;
constexpr int UART_BAUD = 115200;

using uart_wifi_connect_cb_t = void (*)(std::string_view ssid,
                                        std::string_view password);
using uart_start_provisioning_cb_t = void (*)(uint32_t timeout_seconds);
using uart_stop_provisioning_cb_t = void (*)();

void uart_init(uart_wifi_connect_cb_t wifi_cb,
               uart_start_provisioning_cb_t start_prov_cb,
               uart_stop_provisioning_cb_t stop_prov_cb);
void uart_send_wifi_status(bool connected, std::string_view ssid,
                           std::string_view ip_addr, int16_t rssi);
void uart_send_heartbeat_response(uint64_t uptime_ms, uint32_t heap_free);
void uart_send_provisioning_status(CoprocessorProto::ProvisioningState state);

}  // namespace coprocessor
