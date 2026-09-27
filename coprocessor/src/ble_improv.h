// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#pragma once

#include <cstdint>
#include <string_view>

#include "coprocessor_generated.h"

namespace coprocessor {

using ble_wifi_connect_cb_t = void (*)(std::string_view ssid,
                                       std::string_view password);
using ble_status_cb_t = void (*)(CoprocessorProto::ProvisioningState state);

void ble_init(ble_wifi_connect_cb_t connect_cb, ble_status_cb_t status_cb);
void ble_start(uint32_t timeout_seconds);
void ble_stop();
bool ble_is_active();
void ble_on_wifi_connected(std::string_view ip_addr);
void ble_on_wifi_failed();

}  // namespace coprocessor
