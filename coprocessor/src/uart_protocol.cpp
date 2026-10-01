// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#include "uart_protocol.h"

#include <sys/time.h>

#include <array>
#include <cinttypes>
#include <cstring>
#include <string_view>
#include <vector>

#include "driver/uart.h"
#include "esp_log.h"
#include "esp_system.h"
#include "esp_timer.h"
#include "esp_wifi.h"
#include "freertos/task.h"
#include "sonos_controller.h"
#include "web_server.h"
#include "wifi_manager.h"

namespace coprocessor {

namespace {

constexpr const char* TAG = "uart_proto";

bool s_is_linked = false;
uint64_t s_last_s3_uptime = 0;

uart_wifi_connect_cb_t s_wifi_connect_cb = nullptr;
uart_start_provisioning_cb_t s_start_provisioning_cb = nullptr;
uart_stop_provisioning_cb_t s_stop_provisioning_cb = nullptr;

size_t cobs_encode(const uint8_t* src, size_t src_len, uint8_t* dst) {
  size_t read_idx = 0;
  size_t write_idx = 1;
  size_t code_idx = 0;
  uint8_t code = 1;

  while (read_idx < src_len) {
    if (src[read_idx] == 0) {
      dst[code_idx] = code;
      code_idx = write_idx++;
      code = 1;
    } else {
      dst[write_idx++] = src[read_idx];
      code++;
      if (code == 0xFF) {
        dst[code_idx] = code;
        code_idx = write_idx++;
        code = 1;
      }
    }
    read_idx++;
  }
  dst[code_idx] = code;
  return write_idx;
}

size_t cobs_decode(const uint8_t* src, size_t src_len, uint8_t* dst) {
  if (src_len == 0) return 0;
  size_t read_idx = 0;
  size_t write_idx = 0;

  while (read_idx < src_len) {
    uint8_t code = src[read_idx++];
    if (code == 0) return 0;
    for (uint8_t i = 1; i < code; i++) {
      if (read_idx >= src_len) return 0;
      dst[write_idx++] = src[read_idx++];
    }
    if (code < 0xFF && read_idx < src_len) {
      dst[write_idx++] = 0;
    }
  }
  return write_idx;
}

uint32_t crc32_ieee(const uint8_t* data, size_t length) {
  uint32_t crc = 0xFFFFFFFF;
  for (size_t i = 0; i < length; i++) {
    crc ^= data[i];
    for (int k = 0; k < 8; k++) {
      crc = (crc >> 1) ^ (0xEDB88320 & (-static_cast<int>(crc & 1)));
    }
  }
  return ~crc;
}

void send_response_envelope(
    flatbuffers::FlatBufferBuilder& fbb,
    flatbuffers::Offset<CoprocessorProto::ResponseEnvelope> env) {
  fbb.FinishSizePrefixed(env);
  const uint8_t* payload = fbb.GetBufferPointer();
  size_t payload_len = fbb.GetSize();

  uint32_t crc = crc32_ieee(payload, payload_len);

  std::vector<uint8_t> unencoded(4 + payload_len);
  unencoded[0] = static_cast<uint8_t>(crc & 0xFF);
  unencoded[1] = static_cast<uint8_t>((crc >> 8) & 0xFF);
  unencoded[2] = static_cast<uint8_t>((crc >> 16) & 0xFF);
  unencoded[3] = static_cast<uint8_t>((crc >> 24) & 0xFF);
  memcpy(&unencoded[4], payload, payload_len);

  std::vector<uint8_t> encoded(unencoded.size() + unencoded.size() / 254 + 4);
  size_t encoded_len =
      cobs_encode(unencoded.data(), unencoded.size(), encoded.data());
  encoded[encoded_len++] = 0x00;

  uart_write_bytes(UART_PORT, reinterpret_cast<const char*>(encoded.data()),
                   encoded_len);
  ESP_LOGI(TAG, "UART TX: sent response (%u bytes)",
           static_cast<unsigned>(encoded_len));
}

void handle_rx_packet(const uint8_t* payload, size_t payload_len) {
  auto req_env = CoprocessorProto::GetSizePrefixedRequestEnvelope(payload);
  if (!req_env) {
    ESP_LOGW(TAG, "Failed to parse RequestEnvelope");
    return;
  }

  switch (req_env->message_type()) {
    case CoprocessorProto::Request_Hello: {
      if (s_is_linked) {
        ESP_LOGW(TAG,
                 "S3 reboot detected via Hello while linked! Rebooting "
                 "coprocessor...");
        esp_restart();
      } else {
        ESP_LOGI(TAG, "First Hello received from S3, linking session");
        s_is_linked = true;
        uart_send_hello();
        if (wifi_is_connected()) {
          wifi_ap_record_t ap_info;
          int16_t rssi = 0;
          if (esp_wifi_sta_get_ap_info(&ap_info) == ESP_OK) {
            rssi = ap_info.rssi;
          }
          uart_send_wifi_status(true, wifi_get_ssid(), wifi_get_ip(), rssi);
        }
        struct timeval tv;
        gettimeofday(&tv, nullptr);
        if (tv.tv_sec > 1700000000) {
          ESP_LOGI(TAG, "Sending current synced time on Hello link: %lld",
                   static_cast<long long>(tv.tv_sec));
          uart_send_time_sync(static_cast<uint64_t>(tv.tv_sec),
                              static_cast<uint32_t>(tv.tv_usec));
        }
      }
      break;
    }
    case CoprocessorProto::Request_Heartbeat: {
      auto req = req_env->message_as_Heartbeat();
      if (req) {
        if (s_is_linked && s_last_s3_uptime > 0 &&
            req->uptime_ms() < s_last_s3_uptime) {
          ESP_LOGW(TAG,
                   "S3 uptime dropped (%" PRIu64 " < %" PRIu64
                   "), reboot detected! Rebooting coprocessor...",
                   req->uptime_ms(), s_last_s3_uptime);
          esp_restart();
        }
        s_last_s3_uptime = req->uptime_ms();

        if (req->battery()) {
          web_server_update_battery(req->battery());
        }
      }
      auto uptime_ms = static_cast<uint64_t>(esp_timer_get_time() / 1000);
      uint32_t heap_free = esp_get_free_heap_size();
      ESP_LOGI(TAG,
               "Heartbeat request received from S3 (uptime=%" PRIu64
               " ms), sending ACK",
               req ? req->uptime_ms() : 0);
      uart_send_heartbeat_response(uptime_ms, heap_free);
      break;
    }
    case CoprocessorProto::Request_WifiConnectRequest: {
      auto req = req_env->message_as_WifiConnectRequest();
      if (req && s_wifi_connect_cb) {
        std::string_view ssid =
            req->ssid()
                ? std::string_view(req->ssid()->data(), req->ssid()->size())
                : std::string_view{};
        std::string_view pass = req->password()
                                    ? std::string_view(req->password()->data(),
                                                       req->password()->size())
                                    : std::string_view{};
        ESP_LOGI(TAG, "WifiConnectRequest received: ssid='%.*s'",
                 static_cast<int>(ssid.size()), ssid.data());
        s_wifi_connect_cb(ssid, pass);
      }
      break;
    }
    case CoprocessorProto::Request_StartProvisioning: {
      auto req = req_env->message_as_StartProvisioning();
      if (req && s_start_provisioning_cb) {
        ESP_LOGI(TAG, "StartProvisioning request received: timeout=%lu s",
                 static_cast<unsigned long>(req->timeout_seconds()));
        s_start_provisioning_cb(req->timeout_seconds());
      }
      break;
    }
    case CoprocessorProto::Request_StopProvisioning: {
      if (s_stop_provisioning_cb) {
        ESP_LOGI(TAG, "StopProvisioning request received");
        s_stop_provisioning_cb();
      }
      break;
    }
    case CoprocessorProto::Request_BatteryStatus: {
      auto req = req_env->message_as_BatteryStatus();
      if (req) {
        if (req->is_plugged() || req->percent() == 0xFF) {
          ESP_LOGI(TAG, "BatteryStatus received: %lu mV, charging (plugged=%d)",
                   static_cast<unsigned long>(req->millivolts()),
                   static_cast<int>(req->is_plugged()));
        } else {
          ESP_LOGI(TAG, "BatteryStatus received: %lu mV, %u%%, plugged=0",
                   static_cast<unsigned long>(req->millivolts()),
                   static_cast<unsigned>(req->percent()));
        }
        web_server_update_battery(req);
      }
      break;
    }
    case CoprocessorProto::Request_Media_Action: {
      auto req = req_env->message_as_Media_Action();
      if (req && req->endpoint_ip()) {
        if (req->endpoint_ip()->size() == 0 || req->endpoint_port() == 0) {
          ESP_LOGW(TAG, "Media.Action dropped: invalid endpoint");
          break;
        }
        if (!wifi_is_connected()) {
          ESP_LOGW(TAG, "Media.Action dropped: Wi-Fi not connected");
          break;
        }
        ESP_LOGI(TAG, "Media.Action received: ip='%s' port=%u action=%d",
                 req->endpoint_ip()->c_str(),
                 static_cast<unsigned>(req->endpoint_port()),
                 static_cast<int>(req->action()));
        sonos_post_action(req->endpoint_ip()->c_str(), req->endpoint_port(),
                          req->action());
      }
      break;
    }
    case CoprocessorProto::Request_Media_VolumeCommand: {
      auto req = req_env->message_as_Media_VolumeCommand();
      if (req && req->endpoint_ip()) {
        if (req->endpoint_ip()->size() == 0 || req->endpoint_port() == 0) {
          ESP_LOGW(TAG, "Media.VolumeCommand dropped: invalid endpoint");
          break;
        }
        if (!wifi_is_connected()) {
          ESP_LOGW(TAG, "Media.VolumeCommand dropped: Wi-Fi not connected");
          break;
        }
        ESP_LOGI(TAG,
                 "Media.VolumeCommand received: ip='%s' port=%u volume=%d "
                 "relative=%d",
                 req->endpoint_ip()->c_str(),
                 static_cast<unsigned>(req->endpoint_port()),
                 static_cast<int>(req->volume()),
                 static_cast<int>(req->is_relative()));
        sonos_post_volume(req->endpoint_ip()->c_str(), req->endpoint_port(),
                          req->volume(), req->is_relative());
      }
      break;
    }
    case CoprocessorProto::Request_Media_Subscribe: {
      auto req = req_env->message_as_Media_Subscribe();
      if (req && req->endpoint_ip()) {
        if (req->endpoint_ip()->size() == 0 || req->endpoint_port() == 0) {
          ESP_LOGW(TAG, "Media.Subscribe dropped: invalid endpoint");
          break;
        }
        if (!wifi_is_connected()) {
          ESP_LOGW(TAG, "Media.Subscribe dropped: Wi-Fi not connected");
          break;
        }
        ESP_LOGI(TAG, "Media.Subscribe received: ip='%s' port=%u",
                 req->endpoint_ip()->c_str(),
                 static_cast<unsigned>(req->endpoint_port()));
        sonos_post_subscribe(req->endpoint_ip()->c_str(), req->endpoint_port());
      }
      break;
    }
    case CoprocessorProto::Request_Media_UnsubscribeAll: {
      ESP_LOGI(TAG, "Media.UnsubscribeAll received");
      sonos_post_unsubscribe_all();
      break;
    }
    case CoprocessorProto::Request_Media_GetTopology: {
      auto req = req_env->message_as_Media_GetTopology();
      if (req && req->seed_ip()) {
        if (req->seed_ip()->size() == 0 || req->seed_port() == 0) {
          ESP_LOGW(TAG, "Media.GetTopology dropped: invalid seed endpoint");
          break;
        }
        if (!wifi_is_connected()) {
          ESP_LOGW(TAG, "Media.GetTopology dropped: Wi-Fi not connected");
          break;
        }
        ESP_LOGI(TAG, "Media.GetTopology received: seed='%s' port=%u",
                 req->seed_ip()->c_str(),
                 static_cast<unsigned>(req->seed_port()));
        sonos_post_get_topology(req->seed_ip()->c_str(), req->seed_port());
      }
      break;
    }
    default:
      ESP_LOGD(TAG, "Unhandled request type: %d",
               static_cast<int>(req_env->message_type()));
      break;
  }
}

void uart_rx_task(void* pvParameters) {
  constexpr size_t BUFFER_CAPACITY = 2048;
  static std::array<uint8_t, 128> rx_raw{};
  static std::array<uint8_t, BUFFER_CAPACITY> frame_buf{};
  static std::array<uint8_t, BUFFER_CAPACITY> scratch{};
  size_t frame_pos = 0;
  bool frame_overflow = false;

  ESP_LOGI(TAG, "UART RX task started on %d (TX=%d, RX=%d)", UART_PORT,
           UART_TX_PIN, UART_RX_PIN);

  while (true) {
    int len = uart_read_bytes(UART_PORT, rx_raw.data(), rx_raw.size(),
                              pdMS_TO_TICKS(50));
    if (len <= 0) {
      continue;
    }
    ESP_LOGI(TAG, "UART RX: read %d bytes", len);

    for (int i = 0; i < len; i++) {
      uint8_t byte = rx_raw[i];
      if (byte == 0x00) {
        if (frame_overflow) {
          // Tail of an oversized frame: discard up to this delimiter.
          frame_overflow = false;
          frame_pos = 0;
          continue;
        }
        if (frame_pos == 0) {
          continue;  // Skip consecutive delimiters
        }

        size_t decoded_len =
            cobs_decode(frame_buf.data(), frame_pos, scratch.data());
        frame_pos = 0;

        if (decoded_len < 8) {
          ESP_LOGW(TAG, "Frame too short: %u bytes",
                   static_cast<unsigned>(decoded_len));
          continue;
        }

        uint32_t expected_crc = scratch[0] | (scratch[1] << 8) |
                                (scratch[2] << 16) | (scratch[3] << 24);

        const uint8_t* payload = &scratch[4];
        size_t payload_len = decoded_len - 4;
        uint32_t actual_crc = crc32_ieee(payload, payload_len);

        if (expected_crc != actual_crc) {
          ESP_LOGW(TAG, "CRC mismatch (exp 0x%08X != act 0x%08X)",
                   static_cast<unsigned>(expected_crc),
                   static_cast<unsigned>(actual_crc));
          continue;
        }

        uint32_t expected_size = payload[0] | (payload[1] << 8) |
                                 (payload[2] << 16) | (payload[3] << 24);

        if (expected_size != payload_len - 4) {
          ESP_LOGW(TAG, "Size prefix mismatch (exp %u != act %u)",
                   static_cast<unsigned>(expected_size),
                   static_cast<unsigned>(payload_len - 4));
          continue;
        }

        handle_rx_packet(payload, payload_len);
      } else {
        if (frame_overflow) {
          continue;  // Discard the tail of the oversized frame
        }
        if (frame_pos < BUFFER_CAPACITY) {
          frame_buf[frame_pos++] = byte;
        } else {
          ESP_LOGW(TAG, "Frame buffer overflow, discarding until delimiter");
          frame_overflow = true;
          frame_pos = 0;
        }
      }
    }
  }
}

}  // namespace

void uart_send_wifi_status(bool connected, std::string_view ssid,
                           std::string_view ip_addr, int16_t rssi) {
  flatbuffers::FlatBufferBuilder fbb(256);
  auto ssid_str = fbb.CreateString(ssid.data(), ssid.size());
  auto ip_str = fbb.CreateString(ip_addr.data(), ip_addr.size());
  auto wifi_status = CoprocessorProto::CreateWifiStatus(fbb, connected, ip_str,
                                                        ssid_str, rssi);
  auto env = CoprocessorProto::CreateResponseEnvelope(
      fbb, CoprocessorProto::Response_WifiStatus, wifi_status.Union());
  send_response_envelope(fbb, env);
}

void uart_send_heartbeat_response(uint64_t uptime_ms, uint32_t heap_free) {
  flatbuffers::FlatBufferBuilder fbb(128);
  auto hb = CoprocessorProto::CreateHeartbeat(fbb, uptime_ms, heap_free);
  auto env = CoprocessorProto::CreateResponseEnvelope(
      fbb, CoprocessorProto::Response_Heartbeat, hb.Union());
  send_response_envelope(fbb, env);
}

void uart_send_provisioning_status(CoprocessorProto::ProvisioningState state) {
  flatbuffers::FlatBufferBuilder fbb(128);
  auto status = CoprocessorProto::CreateProvisioningStatus(fbb, state);
  auto env = CoprocessorProto::CreateResponseEnvelope(
      fbb, CoprocessorProto::Response_ProvisioningStatus, status.Union());
  send_response_envelope(fbb, env);
}

void uart_send_hello() {
  flatbuffers::FlatBufferBuilder fbb(64);
  auto hello = CoprocessorProto::CreateHello(fbb);
  auto env = CoprocessorProto::CreateResponseEnvelope(
      fbb, CoprocessorProto::Response_Hello, hello.Union());
  send_response_envelope(fbb, env);
}

void uart_send_time_sync(uint64_t epoch_seconds, uint32_t subsec_micros) {
  flatbuffers::FlatBufferBuilder fbb(128);
  auto time_sync =
      CoprocessorProto::CreateTimeSync(fbb, epoch_seconds, subsec_micros);
  auto env = CoprocessorProto::CreateResponseEnvelope(
      fbb, CoprocessorProto::Response_TimeSync, time_sync.Union());
  send_response_envelope(fbb, env);
}

void uart_send_media_state_update(const UartMediaStateUpdate& update) {
  namespace Media = CoprocessorProto::Media;
  flatbuffers::FlatBufferBuilder fbb(512);
  auto ip_str = fbb.CreateString(update.endpoint_ip);

  flatbuffers::Offset<Media::TrackMetadata> track_off = 0;
  if (update.track) {
    const auto& t = *update.track;
    auto title = t.title ? fbb.CreateString(*t.title) : 0;
    auto artist = t.artist ? fbb.CreateString(*t.artist) : 0;
    auto album = t.album ? fbb.CreateString(*t.album) : 0;
    track_off = Media::CreateTrackMetadata(
        fbb, title, artist, album, t.duration_seconds, t.elapsed_seconds);
  }

  auto state_update =
      Media::CreateStateUpdate(fbb, ip_str, update.transport_state,
                               update.volume, update.is_muted, track_off);
  auto env = CoprocessorProto::CreateResponseEnvelope(
      fbb, CoprocessorProto::Response_Media_StateUpdate, state_update.Union());
  send_response_envelope(fbb, env);
}

void uart_send_media_topology_update(
    const std::vector<UartMediaGroup>& groups) {
  namespace Media = CoprocessorProto::Media;
  flatbuffers::FlatBufferBuilder fbb(2048);
  std::vector<flatbuffers::Offset<Media::Group>> group_offsets;
  group_offsets.reserve(groups.size());
  for (const auto& g : groups) {
    std::vector<flatbuffers::Offset<Media::Member>> member_offsets;
    member_offsets.reserve(g.members.size());
    for (const auto& m : g.members) {
      member_offsets.push_back(Media::CreateMemberDirect(
          fbb, m.name.c_str(), m.uuid.c_str(), m.ip.c_str()));
    }
    group_offsets.push_back(Media::CreateGroupDirect(
        fbb, g.id.c_str(), g.name.c_str(), g.coordinator_ip.c_str(),
        g.coordinator_port, false, &member_offsets));
  }
  auto groups_vec = fbb.CreateVector(group_offsets);
  auto topology = Media::CreateTopologyUpdate(fbb, groups_vec);
  auto env = CoprocessorProto::CreateResponseEnvelope(
      fbb, CoprocessorProto::Response_Media_TopologyUpdate, topology.Union());
  send_response_envelope(fbb, env);
}

bool uart_is_linked() { return s_is_linked; }

void uart_init(uart_wifi_connect_cb_t wifi_cb,
               uart_start_provisioning_cb_t start_prov_cb,
               uart_stop_provisioning_cb_t stop_prov_cb) {
  s_wifi_connect_cb = wifi_cb;
  s_start_provisioning_cb = start_prov_cb;
  s_stop_provisioning_cb = stop_prov_cb;
  s_is_linked = false;
  s_last_s3_uptime = 0;

  uart_config_t uart_config = {};
  uart_config.baud_rate = UART_BAUD;
  uart_config.data_bits = UART_DATA_8_BITS;
  uart_config.parity = UART_PARITY_DISABLE;
  uart_config.stop_bits = UART_STOP_BITS_1;
  uart_config.flow_ctrl = UART_HW_FLOWCTRL_DISABLE;
  uart_config.source_clk = UART_SCLK_DEFAULT;

  ESP_ERROR_CHECK(uart_param_config(UART_PORT, &uart_config));
  ESP_ERROR_CHECK(uart_set_pin(UART_PORT, UART_TX_PIN, UART_RX_PIN,
                               UART_PIN_NO_CHANGE, UART_PIN_NO_CHANGE));
  ESP_ERROR_CHECK(uart_driver_install(UART_PORT, 2048, 2048, 0, nullptr, 0));

  xTaskCreatePinnedToCore(uart_rx_task, "uart_rx_task", 4096, nullptr, 10,
                          nullptr, 1);
  uart_send_hello();
}

}  // namespace coprocessor
