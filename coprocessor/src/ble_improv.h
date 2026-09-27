// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#pragma once

#include <cstdint>

#include "coprocessor_generated.h"

namespace coprocessor {

using ble_wifi_connect_cb_t = void (*)(const char* ssid, const char* password);
using ble_status_cb_t = void (*)(CoprocessorProto::ProvisioningState state);

void ble_init(ble_wifi_connect_cb_t connect_cb, ble_status_cb_t status_cb);
void ble_start(uint32_t timeout_seconds);
void ble_stop();
bool ble_is_active();
void ble_on_wifi_connected(const char* ip_addr);
void ble_on_wifi_failed();

}  // namespace coprocessor
