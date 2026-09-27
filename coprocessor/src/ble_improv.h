// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#pragma once

#include <cstdint>
#include "coprocessor_generated.h"

typedef void (*ble_improv_wifi_connect_cb_t)(const char *ssid, const char *password);
typedef void (*ble_improv_status_cb_t)(CoprocessorProto::ProvisioningState state);

void ble_improv_init(ble_improv_wifi_connect_cb_t connect_cb, ble_improv_status_cb_t status_cb);
void ble_improv_start(uint32_t timeout_seconds);
void ble_improv_stop();
bool ble_improv_is_active();
void ble_improv_on_wifi_connected(const char *ip_addr);
void ble_improv_on_wifi_failed();
