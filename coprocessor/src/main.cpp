// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#include <string_view>

#include "ble_improv.h"
#include "esp_log.h"
#include "esp_system.h"
#include "esp_timer.h"
#include "esp_wifi.h"
#include "freertos/FreeRTOS.h"
#include "freertos/timers.h"
#include "nvs_flash.h"
#include "uart_protocol.h"
#include "wifi_manager.h"

namespace {

using namespace coprocessor;

constexpr const char* TAG = "main";

void on_wifi_status_changed(bool connected, std::string_view ssid,
                            std::string_view ip_addr, int16_t rssi) {
  ESP_LOGI(TAG,
           "Wi-Fi status changed: connected=%d, ssid='%.*s', ip='%.*s', "
           "rssi=%d",
           connected, static_cast<int>(ssid.size()), ssid.data(),
           static_cast<int>(ip_addr.size()), ip_addr.data(), rssi);

  // Forward status event to ESP32-S3 over UART
  uart_send_wifi_status(connected, ssid, ip_addr, rssi);

  // Update BLE Improv status if provisioning session is active
  if (ble_is_active()) {
    if (connected) {
      ble_on_wifi_connected(ip_addr);
    } else {
      ble_on_wifi_failed();
    }
  }
}

void on_provisioning_status_changed(CoprocessorProto::ProvisioningState state) {
  ESP_LOGI(TAG, "Provisioning status changed: %d", static_cast<int>(state));
  uart_send_provisioning_status(state);
}

void on_wifi_connect_request(std::string_view ssid, std::string_view password) {
  ESP_LOGI(TAG, "Wi-Fi connect requested for SSID: %.*s",
           static_cast<int>(ssid.size()), ssid.data());
  wifi_connect(ssid, password);
}

void on_start_provisioning_request(uint32_t timeout_seconds) {
  ESP_LOGI(TAG, "Start provisioning requested from S3 (timeout=%u s)",
           static_cast<unsigned>(timeout_seconds));
  ble_start(timeout_seconds);
}

void on_stop_provisioning_request() {
  ESP_LOGI(TAG, "Stop provisioning requested from S3");
  ble_stop();
}

void periodic_status_log_timer(TimerHandle_t xTimer) {
  auto uptime_sec = static_cast<uint32_t>(esp_timer_get_time() / 1000000);
  uint32_t free_heap_kb = esp_get_free_heap_size() / 1024;
  bool connected = wifi_is_connected();
  std::string_view ssid = wifi_get_ssid();
  std::string_view ip = wifi_get_ip();
  int8_t rssi = 0;
  if (connected) {
    wifi_ap_record_t ap_info;
    if (esp_wifi_sta_get_ap_info(&ap_info) == ESP_OK) {
      rssi = ap_info.rssi;
    }
  }
  ESP_LOGI(TAG,
           "[Heartbeat] uptime=%lus, heap=%luKB, wifi=%s (ssid='%.*s', "
           "ip='%.*s', rssi=%d), ble=%s",
           static_cast<unsigned long>(uptime_sec),
           static_cast<unsigned long>(free_heap_kb),
           connected ? "connected" : "disconnected",
           static_cast<int>(ssid.size()), ssid.data(),
           static_cast<int>(ip.size()), ip.data(), rssi,
           ble_is_active() ? "active" : "dormant");
}

}  // namespace

extern "C" void app_main() {
  ESP_LOGI(TAG, "=== Smart Dial Co-Processor Firmware starting ===");

  // Initialize NVS flash storage for Wi-Fi credentials
  esp_err_t ret = nvs_flash_init();
  if (ret == ESP_ERR_NVS_NO_FREE_PAGES ||
      ret == ESP_ERR_NVS_NEW_VERSION_FOUND) {
    ESP_ERROR_CHECK(nvs_flash_erase());
    ret = nvs_flash_init();
  }
  ESP_ERROR_CHECK(ret);

  // Initialize inter-MCU UART to ESP32-S3
  coprocessor::uart_init(on_wifi_connect_request, on_start_provisioning_request,
                         on_stop_provisioning_request);

  // Initialize Improv Wi-Fi BLE GATT service (starts dormant, no advertising)
  coprocessor::ble_init(on_wifi_connect_request,
                        on_provisioning_status_changed);

  // Initialize Wi-Fi station manager
  coprocessor::wifi_init(on_wifi_status_changed);

  // Start 10-second periodic status heartbeat timer
  TimerHandle_t status_timer =
      xTimerCreate("heartbeat_log", pdMS_TO_TICKS(10000), pdTRUE, nullptr,
                   periodic_status_log_timer);
  if (status_timer) {
    xTimerStart(status_timer, 0);
  }

  ESP_LOGI(TAG, "Co-Processor initialized and ready");
}
