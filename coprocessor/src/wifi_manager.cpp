// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#include "wifi_manager.h"

#include <algorithm>
#include <array>
#include <cstring>
#include <string>
#include <string_view>

#include "esp_event.h"
#include "esp_log.h"
#include "esp_netif.h"
#include "esp_wifi.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "nvs.h"

namespace coprocessor {

namespace {

constexpr const char* TAG = "wifi_mgr";

portMUX_TYPE s_wifi_mux = portMUX_INITIALIZER_UNLOCKED;
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

void save_credentials(std::string_view ssid, std::string_view password) {
  nvs_handle_t handle;
  if (nvs_open(NVS_NAMESPACE, NVS_READWRITE, &handle) == ESP_OK) {
    std::string ssid_str(ssid);
    std::string pass_str(password);
    nvs_set_str(handle, NVS_KEY_SSID, ssid_str.c_str());
    nvs_set_str(handle, NVS_KEY_PASS, pass_str.c_str());
    nvs_commit(handle);
    nvs_close(handle);
    ESP_LOGI(TAG, "Saved Wi-Fi credentials for SSID: %.*s",
             static_cast<int>(ssid.size()), ssid.data());
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
          std::string ssid;
          portENTER_CRITICAL(&s_wifi_mux);
          s_connected = false;
          s_ip_str[0] = '\0';
          ssid = s_current_ssid.data();
          portEXIT_CRITICAL(&s_wifi_mux);
          if (s_status_cb) {
            s_status_cb(false, ssid, "", 0);
          }
        }
        break;
      default:
        break;
    }
  } else if (event_base == IP_EVENT && event_id == IP_EVENT_STA_GOT_IP) {
    auto* event = reinterpret_cast<ip_event_got_ip_t*>(event_data);
    std::string ssid;
    std::string ip;
    portENTER_CRITICAL(&s_wifi_mux);
    esp_ip4addr_ntoa(&event->ip_info.ip, s_ip_str.data(), s_ip_str.size());
    s_connected = true;
    ssid = s_current_ssid.data();
    ip = s_ip_str.data();
    portEXIT_CRITICAL(&s_wifi_mux);

    ESP_LOGI(TAG, "Got IP address: %s", ip.c_str());

    int8_t rssi = 0;
    wifi_ap_record_t ap_info;
    if (esp_wifi_sta_get_ap_info(&ap_info) == ESP_OK) {
      rssi = ap_info.rssi;
    }

    if (s_status_cb) {
      s_status_cb(true, ssid, ip, rssi);
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

  ssid.resize(ssid_len);
  password.resize(pass_len);
  nvs_get_str(handle, NVS_KEY_SSID, ssid.data(), &ssid_len);
  nvs_get_str(handle, NVS_KEY_PASS, password.data(), &pass_len);
  nvs_close(handle);

  if (!ssid.empty() && ssid.back() == '\0') {
    ssid.pop_back();
  }
  if (!password.empty() && password.back() == '\0') {
    password.pop_back();
  }

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
    wifi_connect(saved_ssid, saved_pass);
  } else {
    ESP_LOGI(TAG, "No saved Wi-Fi credentials found");
  }
}

void wifi_connect(std::string_view ssid, std::string_view password) {
  if (ssid.empty()) {
    return;
  }

  portENTER_CRITICAL(&s_wifi_mux);
  size_t ssid_copy_len = std::min(ssid.size(), s_current_ssid.size() - 1);
  std::memcpy(s_current_ssid.data(), ssid.data(), ssid_copy_len);
  s_current_ssid[ssid_copy_len] = '\0';
  s_retry_count = 0;
  s_connected = false;
  s_ip_str[0] = '\0';
  portEXIT_CRITICAL(&s_wifi_mux);

  save_credentials(ssid, password);

  wifi_config_t wifi_cfg{};
  size_t cfg_ssid_len = std::min(ssid.size(), sizeof(wifi_cfg.sta.ssid) - 1);
  std::memcpy(wifi_cfg.sta.ssid, ssid.data(), cfg_ssid_len);
  wifi_cfg.sta.ssid[cfg_ssid_len] = '\0';

  if (!password.empty()) {
    size_t cfg_pass_len =
        std::min(password.size(), sizeof(wifi_cfg.sta.password) - 1);
    std::memcpy(wifi_cfg.sta.password, password.data(), cfg_pass_len);
    wifi_cfg.sta.password[cfg_pass_len] = '\0';
    wifi_cfg.sta.threshold.authmode = WIFI_AUTH_WPA2_PSK;
  } else {
    wifi_cfg.sta.threshold.authmode = WIFI_AUTH_OPEN;
  }

  esp_wifi_disconnect();
  esp_wifi_set_config(WIFI_IF_STA, &wifi_cfg);
  esp_wifi_connect();
}

bool wifi_is_connected() {
  portENTER_CRITICAL(&s_wifi_mux);
  bool connected = s_connected;
  portEXIT_CRITICAL(&s_wifi_mux);
  return connected;
}

std::string wifi_get_ip() {
  portENTER_CRITICAL(&s_wifi_mux);
  std::string ip(s_ip_str.data());
  portEXIT_CRITICAL(&s_wifi_mux);
  return ip;
}

std::string wifi_get_ssid() {
  portENTER_CRITICAL(&s_wifi_mux);
  std::string ssid(s_current_ssid.data());
  portEXIT_CRITICAL(&s_wifi_mux);
  return ssid;
}

}  // namespace coprocessor
