// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#include "sntp_service.h"

#include <sys/time.h>

#include "esp_log.h"
#include "esp_netif_sntp.h"
#include "uart_protocol.h"

namespace coprocessor {

namespace {

constexpr const char* TAG = "sntp_svc";
bool s_initialized = false;

void time_sync_notification_cb(struct timeval* tv) {
  ESP_LOGI(TAG, "SNTP time synchronized: sec=%lld, usec=%ld",
           static_cast<long long>(tv->tv_sec), tv->tv_usec);
  // Transmit synchronized time over UART to ESP32-S3
  uart_send_time_sync(static_cast<uint64_t>(tv->tv_sec),
                      static_cast<uint32_t>(tv->tv_usec));
}

}  // namespace

void sntp_service_init() {
  if (s_initialized) {
    return;
  }
  ESP_LOGI(TAG, "Initializing SNTP client (pool.ntp.org)");
  esp_sntp_config_t config = ESP_NETIF_SNTP_DEFAULT_CONFIG("pool.ntp.org");
  config.sync_cb = time_sync_notification_cb;
  config.start = false;  // Start explicitly when Wi-Fi is connected
  ESP_ERROR_CHECK(esp_netif_sntp_init(&config));
  s_initialized = true;
}

void sntp_service_start() {
  if (!s_initialized) {
    sntp_service_init();
  }
  ESP_LOGI(TAG, "Starting SNTP service");
  esp_netif_sntp_start();
}

void sntp_service_stop() {
  if (s_initialized) {
    ESP_LOGI(TAG, "Stopping SNTP service");
    esp_netif_sntp_deinit();
    s_initialized = false;
  }
}

}  // namespace coprocessor
