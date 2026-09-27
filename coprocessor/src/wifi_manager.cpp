// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#include "wifi_manager.h"

#include <array>
#include <cstring>
#include <string>
#include <vector>

#include "esp_event.h"
#include "esp_log.h"
#include "esp_netif.h"
#include "esp_wifi.h"
#include "nvs.h"

namespace coprocessor {

namespace {

constexpr const char* TAG = "wifi_mgr";

wifi_status_cb_t s_status_cb = nullptr;
bool s_connected = false;
std::array<char, 33> s_current_ssid{};
std::array<char, 16> s_ip_str{};
esp_netif_t* s_sta_netif = nullptr;
constexpr int MAX_RETRY_COUNT = 5;
int s_retry_count = 0;

constexpr const char* NVS_NAMESPACE = "wifi_store";
constexpr const char* NVS_KEY_SSID = "ssid";
constexpr const char* NVS_KEY_PASS = "pass";

void save_credentials(const char* ssid, const char* password) {
  nvs_handle_t handle;
  if (nvs_open(NVS_NAMESPACE, NVS_READWRITE, &handle) == ESP_OK) {
    nvs_set_str(handle, NVS_KEY_SSID, ssid);
    nvs_set_str(handle, NVS_KEY_PASS, password);
    nvs_commit(handle);
    nvs_close(handle);
    ESP_LOGI(TAG, "Saved Wi-Fi credentials for SSID: %s", ssid);
  } else {
    ESP_LOGE(TAG, "Failed to open NVS to save Wi-Fi credentials");
  }
}

void wifi_event_handler(void* arg, esp_event_base_t event_base,
                        int32_t event_id, void* event_data) {
  if (event_base == WIFI_EVENT) {
    switch (event_id) {
      case WIFI_EVENT_STA_START:
        ESP_LOGI(TAG, "Wi-Fi STA started");
        break;
      case WIFI_EVENT_STA_CONNECTED:
        ESP_LOGI(TAG, "Wi-Fi connected to AP, waiting for IP...");
        s_retry_count = 0;
        break;
      case WIFI_EVENT_STA_DISCONNECTED:
        if (s_retry_count < MAX_RETRY_COUNT) {
          s_retry_count++;
          ESP_LOGI(TAG, "Wi-Fi disconnected, retrying (%d/%d)...",
                   s_retry_count, MAX_RETRY_COUNT);
          esp_wifi_connect();
        } else {
          ESP_LOGW(TAG, "Wi-Fi disconnected, max retries reached");
          s_connected = false;
          s_ip_str[0] = '\0';
          if (s_status_cb) {
            s_status_cb(false, s_current_ssid.data(), "", 0);
          }
        }
        break;
      default:
        break;
    }
  } else if (event_base == IP_EVENT && event_id == IP_EVENT_STA_GOT_IP) {
    auto* event = reinterpret_cast<ip_event_got_ip_t*>(event_data);
    esp_ip4addr_ntoa(&event->ip_info.ip, s_ip_str.data(), s_ip_str.size());
    ESP_LOGI(TAG, "Got IP address: %s", s_ip_str.data());

    s_connected = true;
    int8_t rssi = 0;
    wifi_ap_record_t ap_info;
    if (esp_wifi_sta_get_ap_info(&ap_info) == ESP_OK) {
      rssi = ap_info.rssi;
    }

    if (s_status_cb) {
      s_status_cb(true, s_current_ssid.data(), s_ip_str.data(), rssi);
    }
  }
}

}  // namespace

bool wifi_get_saved_credentials(std::string& ssid, std::string& password) {
  nvs_handle_t handle;
  if (nvs_open(NVS_NAMESPACE, NVS_READONLY, &handle) != ESP_OK) {
    return false;
  }

  size_t ssid_len = 0;
  size_t pass_len = 0;
  if (nvs_get_str(handle, NVS_KEY_SSID, nullptr, &ssid_len) != ESP_OK ||
      nvs_get_str(handle, NVS_KEY_PASS, nullptr, &pass_len) != ESP_OK) {
    nvs_close(handle);
    return false;
  }

  std::vector<char> ssid_buf(ssid_len);
  std::vector<char> pass_buf(pass_len);
  nvs_get_str(handle, NVS_KEY_SSID, ssid_buf.data(), &ssid_len);
  nvs_get_str(handle, NVS_KEY_PASS, pass_buf.data(), &pass_len);
  nvs_close(handle);

  ssid = ssid_buf.data();
  password = pass_buf.data();
  return !ssid.empty();
}

void wifi_init(wifi_status_cb_t status_cb) {
  s_status_cb = status_cb;

  ESP_ERROR_CHECK(esp_netif_init());
  ESP_ERROR_CHECK(esp_event_loop_create_default());
  s_sta_netif = esp_netif_create_default_wifi_sta();

  wifi_init_config_t cfg = WIFI_INIT_CONFIG_DEFAULT();
  ESP_ERROR_CHECK(esp_wifi_init(&cfg));

  ESP_ERROR_CHECK(esp_event_handler_instance_register(
      WIFI_EVENT, ESP_EVENT_ANY_ID, &wifi_event_handler, nullptr, nullptr));
  ESP_ERROR_CHECK(esp_event_handler_instance_register(
      IP_EVENT, IP_EVENT_STA_GOT_IP, &wifi_event_handler, nullptr, nullptr));

  ESP_ERROR_CHECK(esp_wifi_set_mode(WIFI_MODE_STA));
  ESP_ERROR_CHECK(esp_wifi_start());

  std::string saved_ssid;
  std::string saved_pass;
  if (wifi_get_saved_credentials(saved_ssid, saved_pass)) {
    ESP_LOGI(TAG, "Auto-connecting to saved network: %s", saved_ssid.c_str());
    wifi_connect(saved_ssid.c_str(), saved_pass.c_str());
  } else {
    ESP_LOGI(TAG, "No saved Wi-Fi credentials found");
  }
}

void wifi_connect(const char* ssid, const char* password) {
  if (!ssid || strlen(ssid) == 0) {
    return;
  }

  strncpy(s_current_ssid.data(), ssid, s_current_ssid.size() - 1);
  s_current_ssid.back() = '\0';

  save_credentials(ssid, password);

  wifi_config_t wifi_cfg{};
  strncpy(reinterpret_cast<char*>(wifi_cfg.sta.ssid), ssid,
          sizeof(wifi_cfg.sta.ssid) - 1);
  if (password && strlen(password) > 0) {
    strncpy(reinterpret_cast<char*>(wifi_cfg.sta.password), password,
            sizeof(wifi_cfg.sta.password) - 1);
    wifi_cfg.sta.threshold.authmode = WIFI_AUTH_WPA2_PSK;
  } else {
    wifi_cfg.sta.threshold.authmode = WIFI_AUTH_OPEN;
  }
  s_retry_count = 0;
  s_connected = false;
  s_ip_str[0] = '\0';

  esp_wifi_disconnect();
  esp_wifi_set_config(WIFI_IF_STA, &wifi_cfg);
  esp_wifi_connect();
}

bool wifi_is_connected() { return s_connected; }

const char* wifi_get_ip() { return s_ip_str.data(); }

const char* wifi_get_ssid() { return s_current_ssid.data(); }

}  // namespace coprocessor
