// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#pragma once

#include "esp_err.h"

namespace CoprocessorProto {
struct BatteryStatus;
}  // namespace CoprocessorProto

namespace coprocessor {

/// Starts the embedded HTTP and WebSocket server on port 80.
esp_err_t web_server_start();

/// Stops the HTTP and WebSocket server.
esp_err_t web_server_stop();

/// Returns true if the web server is currently running.
bool web_server_is_running();

/// Updates cached battery state and broadcasts real-time updates to connected
/// WebSocket clients.
void web_server_update_battery(const CoprocessorProto::BatteryStatus* status);

}  // namespace coprocessor
