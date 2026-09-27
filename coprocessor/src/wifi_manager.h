// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#pragma once

#include <cstdint>
#include <string>
#include "coprocessor_generated.h"

using wifi_status_changed_cb_t = void (*)(bool connected,
                                        const char *ssid,
                                        const char *ip_addr,
                                        int16_t rssi);

void wifi_manager_init(wifi_status_changed_cb_t status_cb);
void wifi_manager_connect(const char *ssid, const char *password);
bool wifi_manager_get_saved_credentials(std::string &ssid, std::string &password);
bool wifi_manager_is_connected();
const char *wifi_manager_get_ip();
const char *wifi_manager_get_ssid();
