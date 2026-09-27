// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#pragma once

#include <cstdint>
#include <string>
#include <string_view>

#include "coprocessor_generated.h"

namespace coprocessor {

using wifi_status_cb_t = void (*)(bool connected, std::string_view ssid,
                                  std::string_view ip_addr, int16_t rssi);

void wifi_init(wifi_status_cb_t status_cb);
void wifi_connect(std::string_view ssid, std::string_view password);
bool wifi_get_saved_credentials(std::string& ssid, std::string& password);
bool wifi_is_connected();
std::string_view wifi_get_ip();
std::string_view wifi_get_ssid();

}  // namespace coprocessor
