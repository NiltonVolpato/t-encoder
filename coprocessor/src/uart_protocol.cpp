// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#include "uart_protocol.h"

#include <cstring>
#include <vector>
#include "esp_log.h"
#include "esp_timer.h"
#include "esp_system.h"
#include "driver/uart.h"
#include "freertos/task.h"

static const char *TAG = "uart_proto";

static wifi_connect_request_cb_t s_wifi_connect_cb = nullptr;
static start_provisioning_cb_t s_start_provisioning_cb = nullptr;
static stop_provisioning_cb_t s_stop_provisioning_cb = nullptr;

static size_t cobs_encode(const uint8_t *src, size_t src_len, uint8_t *dst) {
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

static size_t cobs_decode(const uint8_t *src, size_t src_len, uint8_t *dst) {
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

static uint32_t crc32_ieee(const uint8_t *data, size_t length) {
    uint32_t crc = 0xFFFFFFFF;
    for (size_t i = 0; i < length; i++) {
        crc ^= data[i];
        for (int k = 0; k < 8; k++) {
            crc = (crc >> 1) ^ (0xEDB88320 & (-(int)(crc & 1)));
        }
    }
    return ~crc;
}

static void send_response_envelope(flatbuffers::FlatBufferBuilder &fbb,
                                  flatbuffers::Offset<CoprocessorProto::ResponseEnvelope> env) {
    fbb.FinishSizePrefixed(env);
    const uint8_t *payload = fbb.GetBufferPointer();
    size_t payload_len = fbb.GetSize();

    uint32_t crc = crc32_ieee(payload, payload_len);

    std::vector<uint8_t> unencoded(4 + payload_len);
    unencoded[0] = static_cast<uint8_t>(crc & 0xFF);
    unencoded[1] = static_cast<uint8_t>((crc >> 8) & 0xFF);
    unencoded[2] = static_cast<uint8_t>((crc >> 16) & 0xFF);
    unencoded[3] = static_cast<uint8_t>((crc >> 24) & 0xFF);
    memcpy(&unencoded[4], payload, payload_len);

    std::vector<uint8_t> encoded(unencoded.size() + unencoded.size() / 254 + 4);
    size_t encoded_len = cobs_encode(unencoded.data(), unencoded.size(), encoded.data());
    encoded[encoded_len++] = 0x00;

    uart_write_bytes(COPROCESSOR_UART_PORT, reinterpret_cast<const char *>(encoded.data()), encoded_len);
}

void uart_send_wifi_status(bool connected, const char *ssid, const char *ip_addr, int16_t rssi) {
    flatbuffers::FlatBufferBuilder fbb(256);
    auto ssid_str = ssid ? fbb.CreateString(ssid) : fbb.CreateString("");
    auto ip_str = ip_addr ? fbb.CreateString(ip_addr) : fbb.CreateString("");
    auto wifi_status = CoprocessorProto::CreateWifiStatus(fbb, connected, ip_str, ssid_str, rssi);
    auto env = CoprocessorProto::CreateResponseEnvelope(
        fbb,
        CoprocessorProto::Response_WifiStatus,
        wifi_status.Union());
    send_response_envelope(fbb, env);
}

void uart_send_heartbeat_response(uint64_t uptime_ms, uint32_t heap_free) {
    flatbuffers::FlatBufferBuilder fbb(128);
    auto hb = CoprocessorProto::CreateHeartbeat(fbb, uptime_ms, heap_free);
    auto env = CoprocessorProto::CreateResponseEnvelope(
        fbb,
        CoprocessorProto::Response_Heartbeat,
        hb.Union());
    send_response_envelope(fbb, env);
}

void uart_send_provisioning_status(CoprocessorProto::ProvisioningState state) {
    flatbuffers::FlatBufferBuilder fbb(128);
    auto status = CoprocessorProto::CreateProvisioningStatus(fbb, state);
    auto env = CoprocessorProto::CreateResponseEnvelope(
        fbb,
        CoprocessorProto::Response_ProvisioningStatus,
        status.Union());
    send_response_envelope(fbb, env);
}

static void handle_rx_packet(const uint8_t *payload, size_t payload_len) {
    auto req_env = CoprocessorProto::GetSizePrefixedRequestEnvelope(payload);
    if (!req_env) {
        ESP_LOGW(TAG, "Failed to parse RequestEnvelope");
        return;
    }

    switch (req_env->message_type()) {
        case CoprocessorProto::Request_Heartbeat: {
            uint64_t uptime_ms = static_cast<uint64_t>(esp_timer_get_time() / 1000);
            uint32_t heap_free = esp_get_free_heap_size();
            uart_send_heartbeat_response(uptime_ms, heap_free);
            break;
        }
        case CoprocessorProto::Request_WifiConnectRequest: {
            auto req = req_env->message_as_WifiConnectRequest();
            if (req && s_wifi_connect_cb) {
                const char *ssid = req->ssid() ? req->ssid()->c_str() : "";
                const char *pass = req->password() ? req->password()->c_str() : "";
                s_wifi_connect_cb(ssid, pass);
            }
            break;
        }
        case CoprocessorProto::Request_StartProvisioning: {
            auto req = req_env->message_as_StartProvisioning();
            if (req && s_start_provisioning_cb) {
                s_start_provisioning_cb(req->timeout_seconds());
            }
            break;
        }
        case CoprocessorProto::Request_StopProvisioning: {
            if (s_stop_provisioning_cb) {
                s_stop_provisioning_cb();
            }
            break;
        }
        default:
            ESP_LOGD(TAG, "Unhandled request type: %d", static_cast<int>(req_env->message_type()));
            break;
    }
}

static void uart_rx_task(void *pvParameters) {
    static constexpr size_t BUFFER_CAPACITY = 2048;
    static uint8_t rx_raw[128];
    static uint8_t frame_buf[BUFFER_CAPACITY];
    static uint8_t scratch[BUFFER_CAPACITY];
    size_t frame_pos = 0;

    ESP_LOGI(TAG, "UART RX task started on %d (TX=%d, RX=%d)",
             COPROCESSOR_UART_PORT, COPROCESSOR_UART_TX_PIN, COPROCESSOR_UART_RX_PIN);

    while (true) {
        int len = uart_read_bytes(COPROCESSOR_UART_PORT, rx_raw, sizeof(rx_raw), pdMS_TO_TICKS(50));
        if (len <= 0) {
            continue;
        }

        for (int i = 0; i < len; i++) {
            uint8_t byte = rx_raw[i];
            if (byte == 0x00) {
                if (frame_pos == 0) {
                    continue; // Skip consecutive delimiters
                }

                size_t decoded_len = cobs_decode(frame_buf, frame_pos, scratch);
                frame_pos = 0;

                if (decoded_len < 8) {
                    ESP_LOGW(TAG, "Frame too short: %u bytes", static_cast<unsigned>(decoded_len));
                    continue;
                }

                uint32_t expected_crc = scratch[0] |
                                        (scratch[1] << 8) |
                                        (scratch[2] << 16) |
                                        (scratch[3] << 24);

                const uint8_t *payload = &scratch[4];
                size_t payload_len = decoded_len - 4;
                uint32_t actual_crc = crc32_ieee(payload, payload_len);

                if (expected_crc != actual_crc) {
                    ESP_LOGW(TAG, "CRC mismatch (exp 0x%08X != act 0x%08X)",
                             static_cast<unsigned>(expected_crc),
                             static_cast<unsigned>(actual_crc));
                    continue;
                }

                uint32_t expected_size = payload[0] |
                                         (payload[1] << 8) |
                                         (payload[2] << 16) |
                                         (payload[3] << 24);

                if (expected_size != payload_len - 4) {
                    ESP_LOGW(TAG, "Size prefix mismatch (exp %u != act %u)",
                             static_cast<unsigned>(expected_size),
                             static_cast<unsigned>(payload_len - 4));
                    continue;
                }

                handle_rx_packet(payload, payload_len);
            } else {
                if (frame_pos < BUFFER_CAPACITY) {
                    frame_buf[frame_pos++] = byte;
                } else {
                    ESP_LOGW(TAG, "Frame buffer overflow, resetting");
                    frame_pos = 0;
                }
            }
        }
    }
}

void uart_protocol_init(wifi_connect_request_cb_t wifi_cb,
                        start_provisioning_cb_t start_prov_cb,
                        stop_provisioning_cb_t stop_prov_cb) {
    s_wifi_connect_cb = wifi_cb;
    s_start_provisioning_cb = start_prov_cb;
    s_stop_provisioning_cb = stop_prov_cb;

    uart_config_t uart_config = {};
    uart_config.baud_rate = COPROCESSOR_UART_BAUD;
    uart_config.data_bits = UART_DATA_8_BITS;
    uart_config.parity    = UART_PARITY_DISABLE;
    uart_config.stop_bits = UART_STOP_BITS_1;
    uart_config.flow_ctrl = UART_HW_FLOWCTRL_DISABLE;
    uart_config.source_clk = UART_SCLK_DEFAULT;

    ESP_ERROR_CHECK(uart_param_config(COPROCESSOR_UART_PORT, &uart_config));
    ESP_ERROR_CHECK(uart_set_pin(COPROCESSOR_UART_PORT,
                                 COPROCESSOR_UART_TX_PIN,
                                 COPROCESSOR_UART_RX_PIN,
                                 UART_PIN_NO_CHANGE,
                                 UART_PIN_NO_CHANGE));
    ESP_ERROR_CHECK(uart_driver_install(COPROCESSOR_UART_PORT, 2048, 2048, 0, NULL, 0));

    xTaskCreatePinnedToCore(uart_rx_task, "uart_rx_task", 4096, NULL, 10, NULL, 1);
}
