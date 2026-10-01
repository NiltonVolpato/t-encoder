// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#pragma once

#include <cstdint>
#include <string>

#include "media_generated.h"

namespace coprocessor {

/// Initializes the Sonos controller task and its command queue.
/// Call once after wifi_init().
void sonos_controller_init();

/// Posts a transport action (Play/Pause/Next/...) targeting the group
/// coordinator at (ip, port). Safe to call from the UART RX task.
bool sonos_post_action(const char* ip, uint16_t port,
                       CoprocessorProto::Media::ActionType action);

/// Posts a volume command. Absolute volume is clamped to 0..100; relative
/// volume is a signed adjustment.
bool sonos_post_volume(const char* ip, uint16_t port, int16_t volume,
                       bool is_relative);

/// Posts a topology fetch: GetZoneGroupState from the seed speaker, followed
/// by a Media.TopologyUpdate pushed over UART.
bool sonos_post_get_topology(const char* ip, uint16_t port);

/// Posts a GENA subscribe request for all event endpoints on the given
/// coordinator. Any existing subscriptions are unsubscribed first.
bool sonos_post_subscribe(const char* ip, uint16_t port);

/// Posts a request to UNSUBSCRIBE every active GENA subscription.
bool sonos_post_unsubscribe_all();

/// Hands a received GENA NOTIFY body to the Sonos task for parsing.
/// Safe to call from the esp_httpd task.
bool sonos_post_notify(std::string sid, std::string body);

}  // namespace coprocessor
