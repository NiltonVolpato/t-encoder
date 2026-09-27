// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

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

static const char* TAG = "main";

static void on_wifi_status_changed(bool connected, const char* ssid,
                                   const char* ip_addr, int16_t rssi) {
  ESP_LOGI(TAG,
           "Wi-Fi status changed: connected=%d, ssid='%s', ip='%s', rssi=%d",
           connected, ssid, ip_addr, rssi);

  // Forward status event to ESP32-S3 over UART
  uart_send_wifi_status(connected, ssid, ip_addr, rssi);

  // Update BLE Improv status if provisioning session is active
  if (ble_improv_is_active()) {
    if (connected) {
      ble_improv_on_wifi_connected(ip_addr);
    } else {
      ble_improv_on_wifi_failed();
    }
  }
}

static void on_provisioning_status_changed(
    CoprocessorProto::ProvisioningState state) {
  ESP_LOGI(TAG, "Provisioning status changed: %d", static_cast<int>(state));
  uart_send_provisioning_status(state);
}

static void on_wifi_connect_request(const char* ssid, const char* password) {
  ESP_LOGI(TAG, "Wi-Fi connect requested for SSID: %s", ssid);
  wifi_manager_connect(ssid, password);
}

static void on_start_provisioning_request(uint32_t timeout_seconds) {
  ESP_LOGI(TAG, "Start provisioning requested from S3 (timeout=%u s)",
           static_cast<unsigned>(timeout_seconds));
  ble_improv_start(timeout_seconds);
}

static void on_stop_provisioning_request() {
  ESP_LOGI(TAG, "Stop provisioning requested from S3");
  ble_improv_stop();
}

static void periodic_status_log_timer(TimerHandle_t xTimer) {
  auto uptime_sec = static_cast<uint32_t>(esp_timer_get_time() / 1000000);
  uint32_t free_heap_kb = esp_get_free_heap_size() / 1024;
  bool connected = wifi_manager_is_connected();
  const char* ssid = wifi_manager_get_ssid();
  const char* ip = wifi_manager_get_ip();
  int8_t rssi = 0;
  if (connected) {
    wifi_ap_record_t ap_info;
    if (esp_wifi_sta_get_ap_info(&ap_info) == ESP_OK) {
      rssi = ap_info.rssi;
    }
  }
  ESP_LOGI(TAG,
           "[Heartbeat] uptime=%lus, heap=%luKB, wifi=%s (ssid='%s', ip='%s', "
           "rssi=%d), ble=%s",
           static_cast<unsigned long>(uptime_sec),
           static_cast<unsigned long>(free_heap_kb),
           connected ? "connected" : "disconnected", ssid ? ssid : "",
           ip ? ip : "", rssi, ble_improv_is_active() ? "active" : "dormant");
}

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
  uart_protocol_init(on_wifi_connect_request, on_start_provisioning_request,
                     on_stop_provisioning_request);

  // Initialize Improv Wi-Fi BLE GATT service (starts dormant, no advertising)
  ble_improv_init(on_wifi_connect_request, on_provisioning_status_changed);

  // Initialize Wi-Fi station manager
  wifi_manager_init(on_wifi_status_changed);

  // Start 10-second periodic status heartbeat timer
  TimerHandle_t status_timer =
      xTimerCreate("heartbeat_log", pdMS_TO_TICKS(10000), pdTRUE, nullptr,
                   periodic_status_log_timer);
  if (status_timer) {
    xTimerStart(status_timer, 0);
  }

  ESP_LOGI(TAG, "Co-Processor initialized and ready");
}
