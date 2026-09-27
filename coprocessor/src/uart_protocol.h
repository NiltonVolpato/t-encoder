// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#pragma once

#include <cstdint>
#include <cstddef>
#include "coprocessor_generated.h"

// Hardware pinout for ESP32-U4WDH inter-MCU UART to ESP32-S3
#define COPROCESSOR_UART_PORT   UART_NUM_1
#define COPROCESSOR_UART_TX_PIN 18
#define COPROCESSOR_UART_RX_PIN 23
#define COPROCESSOR_UART_BAUD   115200

typedef void (*wifi_connect_request_cb_t)(const char *ssid, const char *password);
typedef void (*start_provisioning_cb_t)(uint32_t timeout_seconds);
typedef void (*stop_provisioning_cb_t)();

void uart_protocol_init(wifi_connect_request_cb_t wifi_cb,
                        start_provisioning_cb_t start_prov_cb,
                        stop_provisioning_cb_t stop_prov_cb);
void uart_send_wifi_status(bool connected, const char *ssid, const char *ip_addr, int16_t rssi);
void uart_send_heartbeat_response(uint64_t uptime_ms, uint32_t heap_free);
void uart_send_provisioning_status(CoprocessorProto::ProvisioningState state);
