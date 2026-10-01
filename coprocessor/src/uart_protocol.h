// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "coprocessor_generated.h"
#include "hal/uart_types.h"
#include "media_generated.h"

namespace coprocessor {

// Hardware pinout for ESP32-U4WDH inter-MCU UART to ESP32-S3
constexpr uart_port_t UART_PORT = UART_NUM_1;
constexpr int UART_TX_PIN = 23;
constexpr int UART_RX_PIN = 18;
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
void uart_send_hello();
void uart_send_time_sync(uint64_t epoch_seconds, uint32_t subsec_micros);
bool uart_is_linked();

struct UartMediaTrackMetadata {
  std::optional<std::string> title;
  std::optional<std::string> artist;
  std::optional<std::string> album;
  std::optional<uint32_t> duration_seconds;
  std::optional<uint32_t> elapsed_seconds;
};

struct UartMediaStateUpdate {
  std::string endpoint_ip;
  std::optional<CoprocessorProto::Media::TransportState> transport_state;
  std::optional<uint8_t> volume;
  std::optional<bool> is_muted;
  std::optional<UartMediaTrackMetadata> track;
};

struct UartMediaMember {
  std::string name;
  std::string uuid;
  std::string ip;
};

struct UartMediaGroup {
  std::string id;
  std::string name;
  std::string coordinator_ip;
  uint16_t coordinator_port = 1400;
  std::vector<UartMediaMember> members;
};

void uart_send_media_state_update(const UartMediaStateUpdate& update);
void uart_send_media_topology_update(const std::vector<UartMediaGroup>& groups);

}  // namespace coprocessor
