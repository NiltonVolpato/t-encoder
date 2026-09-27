// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#pragma once

#include <cstdint>
#include <string>

#include "coprocessor_generated.h"

namespace coprocessor {

using wifi_status_cb_t = void (*)(bool connected, const char* ssid,
                                  const char* ip_addr, int16_t rssi);

void wifi_init(wifi_status_cb_t status_cb);
void wifi_connect(const char* ssid, const char* password);
bool wifi_get_saved_credentials(std::string& ssid, std::string& password);
bool wifi_is_connected();
const char* wifi_get_ip();
const char* wifi_get_ssid();

}  // namespace coprocessor
