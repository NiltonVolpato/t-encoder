// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#include "ble_improv.h"

#include <array>
#include <cstring>
#include <string>
#include <vector>

#include "esp_log.h"
#include "esp_mac.h"
#include "freertos/FreeRTOS.h"
#include "freertos/timers.h"
#include "host/ble_hs.h"
#include "host/ble_uuid.h"
#include "host/util/util.h"
#include "improv.h"
#include "nimble/nimble_port.h"
#include "nimble/nimble_port_freertos.h"
#include "services/gap/ble_svc_gap.h"
#include "services/gatt/ble_svc_gatt.h"

namespace coprocessor {

namespace {

constexpr const char* TAG = "ble_improv";

ble_wifi_connect_cb_t s_connect_cb = nullptr;
ble_status_cb_t s_status_cb = nullptr;
uint8_t s_own_addr_type = 0;

bool s_synced = false;
bool s_active = false;
uint16_t s_conn_handle = BLE_HS_CONN_HANDLE_NONE;

TimerHandle_t s_timeout_timer = nullptr;
TimerHandle_t s_grace_timer = nullptr;

uint8_t s_state = improv::STATE_STOPPED;
uint8_t s_error = improv::ERROR_NONE;
std::vector<uint8_t> s_rpc_result;
std::string s_device_name = "Smart Dial";

uint16_t s_status_val_handle = 0;
uint16_t s_error_val_handle = 0;
uint16_t s_rpc_cmd_val_handle = 0;
uint16_t s_rpc_result_val_handle = 0;
uint16_t s_caps_val_handle = 0;

const ble_uuid128_t s_improv_svc_uuid =
    BLE_UUID128_INIT(0x00, 0x80, 0x26, 0x78, 0x74, 0x27, 0x63, 0x46, 0x72, 0x22,
                     0x28, 0x62, 0x68, 0x77, 0x46, 0x00);

const ble_uuid128_t s_status_chr_uuid =
    BLE_UUID128_INIT(0x01, 0x80, 0x26, 0x78, 0x74, 0x27, 0x63, 0x46, 0x72, 0x22,
                     0x28, 0x62, 0x68, 0x77, 0x46, 0x00);

const ble_uuid128_t s_error_chr_uuid =
    BLE_UUID128_INIT(0x02, 0x80, 0x26, 0x78, 0x74, 0x27, 0x63, 0x46, 0x72, 0x22,
                     0x28, 0x62, 0x68, 0x77, 0x46, 0x00);

const ble_uuid128_t s_rpc_cmd_chr_uuid =
    BLE_UUID128_INIT(0x03, 0x80, 0x26, 0x78, 0x74, 0x27, 0x63, 0x46, 0x72, 0x22,
                     0x28, 0x62, 0x68, 0x77, 0x46, 0x00);

const ble_uuid128_t s_rpc_result_chr_uuid =
    BLE_UUID128_INIT(0x04, 0x80, 0x26, 0x78, 0x74, 0x27, 0x63, 0x46, 0x72, 0x22,
                     0x28, 0x62, 0x68, 0x77, 0x46, 0x00);

const ble_uuid128_t s_caps_chr_uuid =
    BLE_UUID128_INIT(0x05, 0x80, 0x26, 0x78, 0x74, 0x27, 0x63, 0x46, 0x72, 0x22,
                     0x28, 0x62, 0x68, 0x77, 0x46, 0x00);

int gatt_svr_access(uint16_t conn_handle, uint16_t attr_handle,
                    struct ble_gatt_access_ctxt* ctxt, void* arg);

const std::array<struct ble_gatt_chr_def, 6> s_improv_chrs = {
    {{
         // Status Characteristic (00467768-6228-2272-4663-277478268001)
         .uuid = &s_status_chr_uuid.u,
         .access_cb = gatt_svr_access,
         .flags = BLE_GATT_CHR_F_READ | BLE_GATT_CHR_F_NOTIFY,
         .val_handle = &s_status_val_handle,
     },
     {
         // Error Characteristic (00467768-6228-2272-4663-277478268002)
         .uuid = &s_error_chr_uuid.u,
         .access_cb = gatt_svr_access,
         .flags = BLE_GATT_CHR_F_READ | BLE_GATT_CHR_F_NOTIFY,
         .val_handle = &s_error_val_handle,
     },
     {
         // RPC Command Characteristic (00467768-6228-2272-4663-277478268003)
         .uuid = &s_rpc_cmd_chr_uuid.u,
         .access_cb = gatt_svr_access,
         .flags = BLE_GATT_CHR_F_WRITE,
         .val_handle = &s_rpc_cmd_val_handle,
     },
     {
         // RPC Result Characteristic (00467768-6228-2272-4663-277478268004)
         .uuid = &s_rpc_result_chr_uuid.u,
         .access_cb = gatt_svr_access,
         .flags = BLE_GATT_CHR_F_READ | BLE_GATT_CHR_F_NOTIFY,
         .val_handle = &s_rpc_result_val_handle,
     },
     {
         // Capabilities Characteristic (00467768-6228-2272-4663-277478268005)
         .uuid = &s_caps_chr_uuid.u,
         .access_cb = gatt_svr_access,
         .flags = BLE_GATT_CHR_F_READ,
         .val_handle = &s_caps_val_handle,
     },
     {.uuid = nullptr}}};

const std::array<struct ble_gatt_svc_def, 2> s_gatt_svcs = {
    {{
         .type = BLE_GATT_SVC_TYPE_PRIMARY,
         .uuid = &s_improv_svc_uuid.u,
         .characteristics = s_improv_chrs.data(),
     },
     {.type = 0}}};

void ble_advertise();

int ble_gap_event(struct ble_gap_event* event, void* arg) {
  switch (event->type) {
    case BLE_GAP_EVENT_CONNECT:
      ESP_LOGI(TAG, "BLE connection %s; status=%d",
               event->connect.status == 0 ? "established" : "failed",
               event->connect.status);
      if (event->connect.status == 0) {
        s_conn_handle = event->connect.conn_handle;
      } else {
        s_conn_handle = BLE_HS_CONN_HANDLE_NONE;
        if (s_active) {
          ble_advertise();
        }
      }
      break;
    case BLE_GAP_EVENT_DISCONNECT:
      ESP_LOGI(TAG, "BLE disconnected; reason=%d", event->disconnect.reason);
      s_conn_handle = BLE_HS_CONN_HANDLE_NONE;
      if (s_active && s_state != improv::STATE_PROVISIONED) {
        ble_advertise();
      }
      break;
    case BLE_GAP_EVENT_ADV_COMPLETE:
      if (s_active && s_conn_handle == BLE_HS_CONN_HANDLE_NONE) {
        ble_advertise();
      }
      break;
    default:
      break;
  }
  return 0;
}

void ble_advertise() {
  if (!s_active || !s_synced) {
    return;
  }

  struct ble_gap_adv_params adv_params;
  memset(&adv_params, 0, sizeof(adv_params));
  adv_params.conn_mode = BLE_GAP_CONN_MODE_UND;
  adv_params.disc_mode = BLE_GAP_DISC_MODE_GEN;

  struct ble_hs_adv_fields adv_fields;
  memset(&adv_fields, 0, sizeof(adv_fields));

  adv_fields.flags = BLE_HS_ADV_F_DISC_GEN | BLE_HS_ADV_F_BREDR_UNSUP;
  adv_fields.uuids128 = &s_improv_svc_uuid;
  adv_fields.num_uuids128 = 1;
  adv_fields.uuids128_is_complete = 1;

  int rc = ble_gap_adv_set_fields(&adv_fields);
  if (rc != 0) {
    ESP_LOGE(TAG, "Error setting advertisement fields: rc=%d", rc);
    return;
  }

  struct ble_hs_adv_fields rsp_fields;
  memset(&rsp_fields, 0, sizeof(rsp_fields));
  const char* name = ble_svc_gap_device_name();
  rsp_fields.name = reinterpret_cast<const uint8_t*>(name);
  rsp_fields.name_len = strlen(name);
  rsp_fields.name_is_complete = 1;

  rc = ble_gap_adv_rsp_set_fields(&rsp_fields);
  if (rc != 0) {
    ESP_LOGE(TAG, "Error setting scan response fields: rc=%d", rc);
    return;
  }

  rc = ble_gap_adv_start(s_own_addr_type, nullptr, BLE_HS_FOREVER, &adv_params,
                         ble_gap_event, nullptr);
  if (rc != 0) {
    ESP_LOGE(TAG, "Error starting advertising: rc=%d", rc);
  } else {
    ESP_LOGI(TAG, "BLE advertising started for Improv Wi-Fi");
  }
}

int gatt_svr_access(uint16_t conn_handle, uint16_t attr_handle,
                    struct ble_gatt_access_ctxt* ctxt, void* arg) {
  if (ctxt->op == BLE_GATT_ACCESS_OP_READ_CHR) {
    if (attr_handle == s_status_val_handle) {
      return os_mbuf_append(ctxt->om, &s_state, 1) == 0
                 ? 0
                 : BLE_ATT_ERR_INSUFFICIENT_RES;
    } else if (attr_handle == s_error_val_handle) {
      return os_mbuf_append(ctxt->om, &s_error, 1) == 0
                 ? 0
                 : BLE_ATT_ERR_INSUFFICIENT_RES;
    } else if (attr_handle == s_rpc_result_val_handle) {
      return os_mbuf_append(ctxt->om, s_rpc_result.data(),
                            s_rpc_result.size()) == 0
                 ? 0
                 : BLE_ATT_ERR_INSUFFICIENT_RES;
    } else if (attr_handle == s_caps_val_handle) {
      uint8_t caps = 0;
      return os_mbuf_append(ctxt->om, &caps, 1) == 0
                 ? 0
                 : BLE_ATT_ERR_INSUFFICIENT_RES;
    }
  } else if (ctxt->op == BLE_GATT_ACCESS_OP_WRITE_CHR) {
    if (attr_handle == s_rpc_cmd_val_handle) {
      uint16_t len = OS_MBUF_PKTLEN(ctxt->om);
      std::vector<uint8_t> data(len);
      int rc = os_mbuf_copydata(ctxt->om, 0, len, data.data());
      if (rc != 0) {
        return BLE_ATT_ERR_UNLIKELY;
      }

      improv::ImprovCommand cmd = improv::parse_improv_data(data);
      switch (cmd.command) {
        case improv::Command::WIFI_SETTINGS: {
          ESP_LOGI(TAG, "Improv received Wi-Fi settings: SSID='%s'",
                   cmd.ssid.c_str());
          s_state = improv::STATE_PROVISIONING;
          s_error = improv::ERROR_NONE;
          ble_gatts_chr_updated(s_status_val_handle);

          if (s_connect_cb) {
            s_connect_cb(cmd.ssid.c_str(), cmd.password.c_str());
          }
          break;
        }
        case improv::Command::GET_CURRENT_STATE: {
          ble_gatts_chr_updated(s_status_val_handle);
          break;
        }
        case improv::Command::GET_DEVICE_INFO: {
          std::vector<std::string> info = {
              "smart-dial", "1.0.0", "waveshare knob 1.8/esp32", s_device_name};
          s_rpc_result = improv::build_rpc_response(
              improv::Command::GET_DEVICE_INFO, info);
          ble_gatts_chr_updated(s_rpc_result_val_handle);
          break;
        }
        default:
          ESP_LOGW(TAG, "Unhandled Improv command: %d",
                   static_cast<int>(cmd.command));
          s_error = improv::ERROR_UNKNOWN_RPC;
          ble_gatts_chr_updated(s_error_val_handle);
          break;
      }
      return 0;
    }
  }
  return BLE_ATT_ERR_UNLIKELY;
}

void on_timeout_timer(TimerHandle_t xTimer) {
  ESP_LOGI(TAG, "Provisioning window timed out after window expired");
  if (s_active) {
    s_active = false;
    ble_gap_adv_stop();
    if (s_conn_handle != BLE_HS_CONN_HANDLE_NONE) {
      ble_gap_terminate(s_conn_handle, BLE_ERR_REM_USER_CONN_TERM);
      s_conn_handle = BLE_HS_CONN_HANDLE_NONE;
    }
    if (s_status_cb) {
      s_status_cb(CoprocessorProto::ProvisioningState_TimedOut);
    }
  }
}

void on_grace_timer(TimerHandle_t xTimer) {
  ESP_LOGI(TAG, "Provisioning grace period ended, turning off BLE");
  if (s_active) {
    s_active = false;
    ble_gap_adv_stop();
    if (s_conn_handle != BLE_HS_CONN_HANDLE_NONE) {
      ble_gap_terminate(s_conn_handle, BLE_ERR_REM_USER_CONN_TERM);
      s_conn_handle = BLE_HS_CONN_HANDLE_NONE;
    }
    if (s_status_cb) {
      s_status_cb(CoprocessorProto::ProvisioningState_Inactive);
    }
  }
}

void ble_on_sync() {
  int rc = ble_hs_util_ensure_addr(0);
  assert(rc == 0);
  rc = ble_hs_id_infer_auto(0, &s_own_addr_type);
  assert(rc == 0);
  s_synced = true;

  // Only advertise if an explicit provisioning session was requested
  if (s_active) {
    ble_advertise();
  }
}

void ble_host_task(void* param) {
  ESP_LOGI(TAG, "BLE Host Task Started");
  nimble_port_run();
  nimble_port_freertos_deinit();
}

}  // namespace

bool ble_is_active() { return s_active; }

void ble_start(uint32_t timeout_seconds) {
  if (timeout_seconds == 0) {
    timeout_seconds = 180;
  }
  ESP_LOGI(TAG, "Starting BLE Improv provisioning session (timeout: %u s)",
           static_cast<unsigned>(timeout_seconds));

  s_active = true;
  s_state = improv::STATE_AUTHORIZED;
  s_error = improv::ERROR_NONE;

  if (s_grace_timer) {
    xTimerStop(s_grace_timer, 0);
  }
  if (s_timeout_timer) {
    xTimerChangePeriod(s_timeout_timer, pdMS_TO_TICKS(timeout_seconds * 1000),
                       0);
    xTimerStart(s_timeout_timer, 0);
  }

  if (s_synced) {
    ble_advertise();
  }

  if (s_status_cb) {
    s_status_cb(CoprocessorProto::ProvisioningState_Active);
  }
}

void ble_stop() {
  if (!s_active) {
    return;
  }
  ESP_LOGI(TAG, "Stopping BLE Improv provisioning session");

  s_active = false;
  if (s_timeout_timer) {
    xTimerStop(s_timeout_timer, 0);
  }
  if (s_grace_timer) {
    xTimerStop(s_grace_timer, 0);
  }

  ble_gap_adv_stop();
  if (s_conn_handle != BLE_HS_CONN_HANDLE_NONE) {
    ble_gap_terminate(s_conn_handle, BLE_ERR_REM_USER_CONN_TERM);
    s_conn_handle = BLE_HS_CONN_HANDLE_NONE;
  }

  if (s_status_cb) {
    s_status_cb(CoprocessorProto::ProvisioningState_Inactive);
  }
}

void ble_on_wifi_connected(const char* ip_addr) {
  if (!s_active || s_status_val_handle == 0) {
    return;
  }

  s_state = improv::STATE_PROVISIONED;
  s_error = improv::ERROR_NONE;

  std::string url = std::string("http://") + (ip_addr ? ip_addr : "");
  std::vector<std::string> urls = {url};
  s_rpc_result =
      improv::build_rpc_response(improv::Command::WIFI_SETTINGS, urls);

  ble_gatts_chr_updated(s_rpc_result_val_handle);
  ble_gatts_chr_updated(s_status_val_handle);
  ESP_LOGI(TAG, "Improv state updated to PROVISIONED with URL: %s",
           url.c_str());

  if (s_status_cb) {
    s_status_cb(CoprocessorProto::ProvisioningState_Provisioned);
  }

  // Give 10 seconds for the browser to read final status and redirect URL
  if (s_grace_timer) {
    xTimerStart(s_grace_timer, 0);
  }
}

void ble_on_wifi_failed() {
  if (!s_active || s_status_val_handle == 0) {
    return;
  }

  s_state = improv::STATE_AUTHORIZED;
  s_error = improv::ERROR_UNABLE_TO_CONNECT;

  ble_gatts_chr_updated(s_error_val_handle);
  ble_gatts_chr_updated(s_status_val_handle);
  ESP_LOGW(TAG, "Improv state updated to ERROR_UNABLE_TO_CONNECT");
}

void ble_init(ble_wifi_connect_cb_t connect_cb, ble_status_cb_t status_cb) {
  s_connect_cb = connect_cb;
  s_status_cb = status_cb;
  s_active = false;
  s_synced = false;

  s_timeout_timer = xTimerCreate("prov_to", pdMS_TO_TICKS(180000), pdFALSE,
                                 nullptr, on_timeout_timer);
  s_grace_timer = xTimerCreate("prov_gr", pdMS_TO_TICKS(10000), pdFALSE,
                               nullptr, on_grace_timer);

  ESP_ERROR_CHECK(nimble_port_init());

  ble_hs_cfg.sync_cb = ble_on_sync;

  std::array<uint8_t, 6> mac{};
  if (esp_read_mac(mac.data(), ESP_MAC_WIFI_STA) == ESP_OK) {
    std::array<char, 32> name_buf{};
    snprintf(name_buf.data(), name_buf.size(), "Smart Dial %02X%02X", mac[4],
             mac[5]);
    s_device_name = name_buf.data();
  }

  ble_svc_gap_init();
  ble_svc_gatt_init();

  ESP_ERROR_CHECK(ble_svc_gap_device_name_set(s_device_name.c_str()));
  ESP_LOGI(TAG, "Device name set to: %s", s_device_name.c_str());

  int rc = ble_gatts_count_cfg(s_gatt_svcs.data());
  assert(rc == 0);
  rc = ble_gatts_add_svcs(s_gatt_svcs.data());
  assert(rc == 0);

  nimble_port_freertos_init(ble_host_task);
}

}  // namespace coprocessor
