// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#include "web_server.h"

#include <sys/time.h>
#include <time.h>

#include <atomic>
#include <cstdio>
#include <string>

#include "coprocessor_generated.h"
#include "esp_http_server.h"
#include "esp_log.h"
#include "esp_system.h"
#include "esp_timer.h"
#include "esp_wifi.h"
#include "sonos_controller.h"
#include "wifi_manager.h"

namespace {

constexpr const char* TAG = "web_server";

httpd_handle_t g_server = nullptr;

std::atomic<uint32_t> g_battery_millivolts{0};
std::atomic<int32_t> g_battery_percent{-1};
std::atomic<bool> g_battery_plugged{false};

constexpr const char INDEX_HTML[] = R"rawliteral(<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Smart Dial Dashboard</title>
  <style>
    :root {
      --bg: #090d16;
      --card-bg: #131b2e;
      --card-border: #1e293b;
      --text: #f8fafc;
      --text-muted: #94a3b8;
      --accent: #38bdf8;
      --green: #10b981;
      --red: #ef4444;
      --amber: #f59e0b;
    }
    * { box-sizing: border-box; margin: 0; padding: 0; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; }
    body { background: var(--bg); color: var(--text); padding: 24px; display: flex; flex-direction: column; align-items: center; min-height: 100vh; }
    .container { width: 100%; max-width: 720px; }
    header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 24px; }
    h1 { font-size: 22px; font-weight: 700; color: var(--text); }
    .badge { display: inline-flex; align-items: center; gap: 6px; padding: 4px 10px; border-radius: 9999px; font-size: 12px; font-weight: 600; }
    .badge-live { background: rgba(16, 185, 129, 0.15); color: var(--green); border: 1px solid var(--green); }
    .badge-connecting { background: rgba(245, 158, 11, 0.15); color: var(--amber); border: 1px solid var(--amber); }
    .dot { width: 7px; height: 7px; border-radius: 50%; background: currentColor; }
    .grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(300px, 1fr)); gap: 16px; margin-bottom: 24px; }
    .card { background: var(--card-bg); border: 1px solid var(--card-border); border-radius: 16px; padding: 20px; display: flex; flex-direction: column; gap: 14px; box-shadow: 0 4px 20px rgba(0,0,0,0.3); }
    .card-header { display: flex; justify-content: space-between; align-items: center; }
    .card-title { font-size: 14px; font-weight: 600; text-transform: uppercase; letter-spacing: 0.8px; color: var(--text-muted); }
    .stat-main { font-size: 32px; font-weight: 800; color: var(--text); }
    .stat-sub { font-size: 14px; color: var(--text-muted); }
    .progress-bar-bg { width: 100%; height: 8px; background: rgba(255,255,255,0.08); border-radius: 4px; overflow: hidden; margin-top: 4px; }
    .progress-bar-fill { height: 100%; background: var(--green); transition: width 0.3s ease; }
    .info-row { display: flex; justify-content: space-between; font-size: 14px; padding: 6px 0; border-bottom: 1px solid rgba(255,255,255,0.04); }
    .info-row:last-child { border-bottom: none; }
    .info-label { color: var(--text-muted); }
    .info-val { font-weight: 600; color: var(--text); }
  </style>
</head>
<body>
  <div class="container">
    <header>
      <h1>Waveshare Smart Dial</h1>
      <div id="ws-badge" class="badge badge-connecting">
        <span class="dot"></span>
        <span id="ws-text">Connecting...</span>
      </div>
    </header>

    <div class="grid">
      <!-- Battery Card -->
      <div class="card">
        <div class="card-header">
          <span class="card-title">Battery & Power</span>
          <span id="bat-plugged" class="badge" style="background: rgba(56,189,248,0.15); color: var(--accent);">--</span>
        </div>
        <div>
          <div id="bat-pct" class="stat-main">--%</div>
          <div id="bat-mv" class="stat-sub">-- mV</div>
          <div class="progress-bar-bg">
            <div id="bat-bar" class="progress-bar-fill" style="width: 0%;"></div>
          </div>
        </div>
      </div>

      <!-- Wi-Fi Card -->
      <div class="card">
        <div class="card-header">
          <span class="card-title">Wi-Fi Network</span>
          <span id="wifi-status" class="badge badge-live">Connected</span>
        </div>
        <div class="info-row">
          <span class="info-label">SSID</span>
          <span id="wifi-ssid" class="info-val">--</span>
        </div>
        <div class="info-row">
          <span class="info-label">IP Address</span>
          <span id="wifi-ip" class="info-val">--</span>
        </div>
        <div class="info-row">
          <span class="info-label">Signal (RSSI)</span>
          <span id="wifi-rssi" class="info-val">-- dBm</span>
        </div>
      </div>

      <!-- System Resources Card -->
      <div class="card">
        <div class="card-header">
          <span class="card-title">Co-Processor Memory</span>
        </div>
        <div class="info-row">
          <span class="info-label">Free Heap</span>
          <span id="sys-heap" class="info-val">-- KB</span>
        </div>
        <div class="info-row">
          <span class="info-label">Minimum Free Heap</span>
          <span id="sys-min-heap" class="info-val">-- KB</span>
        </div>
        <div class="info-row">
          <span class="info-label">Uptime</span>
          <span id="sys-uptime" class="info-val">--</span>
        </div>
      </div>

      <!-- Synchronized Time Card -->
      <div class="card">
        <div class="card-header">
          <span class="card-title">Network Time (SNTP)</span>
        </div>
        <div id="time-formatted" class="stat-main" style="font-size: 20px; font-weight: 600;">--</div>
        <div class="info-row">
          <span class="info-label">Epoch Seconds</span>
          <span id="time-epoch" class="info-val">--</span>
        </div>
      </div>
    </div>
  </div>

  <script>
    function updateUI(data) {
      if (data.battery) {
        const b = data.battery;
        document.getElementById('bat-pct').innerText = b.is_plugged ? '⚡ Charging' : (b.percent >= 0 ? `${b.percent}%` : '--%');
        document.getElementById('bat-mv').innerText = b.millivolts > 0 ? `${b.millivolts} mV` : '-- mV';
        const bar = document.getElementById('bat-bar');
        if (b.is_plugged) {
          bar.style.width = '100%';
          bar.style.background = 'var(--accent)';
        } else {
          bar.style.width = `${Math.min(100, Math.max(0, b.percent))}%`;
          bar.style.background = b.percent <= 20 ? 'var(--red)' : 'var(--green)';
        }
        
        const plugBadge = document.getElementById('bat-plugged');
        if (b.is_plugged) {
          plugBadge.innerText = '⚡ Plugged In';
          plugBadge.style.color = 'var(--accent)';
          plugBadge.style.borderColor = 'var(--accent)';
        } else {
          plugBadge.innerText = '🔋 On Battery';
          plugBadge.style.color = 'var(--green)';
          plugBadge.style.borderColor = 'var(--green)';
        }
      }

      if (data.wifi) {
        document.getElementById('wifi-ssid').innerText = data.wifi.ssid || '--';
        document.getElementById('wifi-ip').innerText = data.wifi.ip || '--';
        document.getElementById('wifi-rssi').innerText = `${data.wifi.rssi} dBm`;
      }

      if (data.system) {
        document.getElementById('sys-heap').innerText = `${(data.system.free_heap_bytes / 1024).toFixed(1)} KB`;
        document.getElementById('sys-min-heap').innerText = `${(data.system.min_free_heap_bytes / 1024).toFixed(1)} KB`;
        const s = data.system.uptime_seconds;
        const hrs = Math.floor(s / 3600);
        const mins = Math.floor((s % 3600) / 60);
        const secs = s % 60;
        document.getElementById('sys-uptime').innerText = `${hrs}h ${mins}m ${secs}s`;
      }

      if (data.time) {
        document.getElementById('time-formatted').innerText = data.time.formatted || '--';
        document.getElementById('time-epoch').innerText = data.time.epoch_seconds || '--';
      }
    }

    // Initial HTTP fetch
    fetch('/api/status').then(r => r.json()).then(updateUI).catch(console.error);

    // WebSocket connection for real-time live push updates
    function connectWs() {
      const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
      const ws = new WebSocket(`${proto}//${location.host}/ws`);
      const badge = document.getElementById('ws-badge');
      const text = document.getElementById('ws-text');

      ws.onopen = () => {
        badge.className = 'badge badge-live';
        text.innerText = 'LIVE (WebSocket)';
      };

      ws.onmessage = (event) => {
        try {
          const data = JSON.parse(event.data);
          updateUI(data);
        } catch (e) {
          console.error('WS parse error:', e);
        }
      };

      ws.onclose = () => {
        badge.className = 'badge badge-connecting';
        text.innerText = 'Reconnecting...';
        setTimeout(connectWs, 2000);
      };

      ws.onerror = () => ws.close();
    }

    connectWs();
  </script>
</body>
</html>
)rawliteral";

std::string build_status_json() {
  uint32_t mv = g_battery_millivolts.load();
  int32_t pct = g_battery_percent.load();
  bool plugged = g_battery_plugged.load();

  bool wifi_conn = coprocessor::wifi_is_connected();
  std::string ssid = coprocessor::wifi_get_ssid();
  std::string ip = coprocessor::wifi_get_ip();
  int8_t rssi = 0;
  if (wifi_conn) {
    wifi_ap_record_t ap_info = {};
    if (esp_wifi_sta_get_ap_info(&ap_info) == ESP_OK) {
      rssi = ap_info.rssi;
    }
  }

  uint32_t uptime_seconds =
      static_cast<uint32_t>(esp_timer_get_time() / 1000000);
  uint32_t free_heap = esp_get_free_heap_size();
  uint32_t min_free_heap = esp_get_minimum_free_heap_size();

  time_t now = 0;
  time(&now);
  struct tm timeinfo = {};
  gmtime_r(&now, &timeinfo);
  char time_buffer[64] = {};
  strftime(time_buffer, sizeof(time_buffer), "%Y-%m-%d %H:%M:%S UTC",
           &timeinfo);

  char buffer[512] = {};
  int written = snprintf(
      buffer, sizeof(buffer),
      "{\"battery\":{\"millivolts\":%lu,\"percent\":%d,\"is_plugged\":%s},"
      "\"wifi\":{\"connected\":%s,\"ssid\":\"%.*s\",\"ip\":\"%.*s\",\"rssi\":"
      "%d},"
      "\"system\":{\"uptime_seconds\":%lu,\"free_heap_bytes\":%lu,\"min_free_"
      "heap_bytes\":%lu},"
      "\"time\":{\"epoch_seconds\":%llu,\"formatted\":\"%s\"}}",
      static_cast<unsigned long>(mv), static_cast<int>(pct),
      plugged ? "true" : "false", wifi_conn ? "true" : "false",
      static_cast<int>(ssid.size()), ssid.data(), static_cast<int>(ip.size()),
      ip.data(), rssi, static_cast<unsigned long>(uptime_seconds),
      static_cast<unsigned long>(free_heap),
      static_cast<unsigned long>(min_free_heap),
      static_cast<unsigned long long>(now), time_buffer);

  if (written > 0 && static_cast<size_t>(written) < sizeof(buffer)) {
    return std::string(buffer, written);
  }
  return "{}";
}

struct WsBroadcastData {
  std::string json;
};

void ws_broadcast_work(void* arg) {
  auto* data = static_cast<WsBroadcastData*>(arg);
  if (!data) {
    return;
  }

  if (g_server) {
    httpd_ws_frame_t ws_frame = {};
    ws_frame.type = HTTPD_WS_TYPE_TEXT;
    ws_frame.payload = reinterpret_cast<uint8_t*>(data->json.data());
    ws_frame.len = data->json.size();

    size_t max_clients = 8;
    int client_fds[8] = {};
    if (httpd_get_client_list(g_server, &max_clients, client_fds) == ESP_OK) {
      for (size_t i = 0; i < max_clients; ++i) {
        int fd = client_fds[i];
        if (httpd_ws_get_fd_info(g_server, fd) == HTTPD_WS_CLIENT_WEBSOCKET) {
          httpd_ws_send_frame_async(g_server, fd, &ws_frame);
        }
      }
    }
  }

  delete data;
}

void broadcast_status() {
  if (!g_server) {
    return;
  }
  auto* data = new WsBroadcastData{build_status_json()};
  esp_err_t ret = httpd_queue_work(g_server, ws_broadcast_work, data);
  if (ret != ESP_OK) {
    delete data;
  }
}

esp_err_t index_get_handler(httpd_req_t* req) {
  httpd_resp_set_type(req, "text/html; charset=utf-8");
  return httpd_resp_send(req, INDEX_HTML, HTTPD_RESP_USE_STRLEN);
}

esp_err_t status_get_handler(httpd_req_t* req) {
  std::string json = build_status_json();
  httpd_resp_set_type(req, "application/json");
  return httpd_resp_send(req, json.data(), json.size());
}

#ifdef CONFIG_HTTPD_WS_POST_HANDSHAKE_CB_SUPPORT
esp_err_t ws_post_handshake_cb(httpd_req_t* req) {
  ESP_LOGI(TAG,
           "New WebSocket client connected (fd=%d), sending initial status",
           httpd_req_to_sockfd(req));
  std::string json = build_status_json();
  httpd_ws_frame_t ws_pkt = {};
  ws_pkt.type = HTTPD_WS_TYPE_TEXT;
  ws_pkt.payload = reinterpret_cast<uint8_t*>(json.data());
  ws_pkt.len = json.size();
  return httpd_ws_send_frame(req, &ws_pkt);
}
#endif

esp_err_t ws_handler(httpd_req_t* req) {
  httpd_ws_frame_t ws_pkt = {};
  uint8_t buffer[64] = {};
  ws_pkt.payload = buffer;
  esp_err_t ret = httpd_ws_recv_frame(req, &ws_pkt, sizeof(buffer) - 1);
  if (ret != ESP_OK) {
    ESP_LOGW(TAG, "httpd_ws_recv_frame failed: %d", ret);
    return ret;
  }
  return ESP_OK;
}

// Spam guard state for the GENA NOTIFY endpoint (rate detection only, no
// throttling).
uint32_t g_notify_window_start_ms = 0;
uint32_t g_notify_count = 0;
bool g_notify_warned = false;

// Handles UPnP GENA NOTIFY requests from subscribed Sonos speakers. The body
// is forwarded to the Sonos task for parsing; nothing heavy runs here.
esp_err_t media_notify_handler(httpd_req_t* req) {
  uint32_t now_ms = static_cast<uint32_t>(esp_timer_get_time() / 1000);
  if (now_ms - g_notify_window_start_ms >= 1000) {
    g_notify_window_start_ms = now_ms;
    g_notify_count = 0;
    g_notify_warned = false;
  }
  g_notify_count++;
  if (g_notify_count > 10 && !g_notify_warned) {
    g_notify_warned = true;
    ESP_LOGW(TAG, "NOTIFY flood detected: >10 requests/second");
  }

  char sid[160] = {};
  if (httpd_req_get_hdr_value_str(req, "SID", sid, sizeof(sid)) != ESP_OK) {
    sid[0] = '\0';
  }

  std::string body;
  int remaining = req->content_len;
  if (remaining > 16 * 1024) {
    ESP_LOGW(TAG, "NOTIFY body too large (%d bytes), rejecting", remaining);
    return httpd_resp_send_err(req, HTTPD_400_BAD_REQUEST, "body too large");
  }
  if (remaining > 0) {
    body.reserve(remaining);
    char buf[256];
    while (remaining > 0) {
      int chunk = remaining < static_cast<int>(sizeof(buf))
                      ? remaining
                      : static_cast<int>(sizeof(buf));
      int n = httpd_req_recv(req, buf, chunk);
      if (n <= 0) {
        if (n == HTTPD_SOCK_ERR_TIMEOUT) {
          continue;
        }
        ESP_LOGW(TAG, "NOTIFY body recv failed: %d", n);
        return httpd_resp_send_err(req, HTTPD_500_INTERNAL_SERVER_ERROR,
                                   "recv failed");
      }
      body.append(buf, n);
      remaining -= n;
    }
  }

  // Acknowledge promptly, then hand off to the Sonos task.
  esp_err_t ret = httpd_resp_send(req, nullptr, 0);
  coprocessor::sonos_post_notify(sid, std::move(body));
  return ret;
}

}  // namespace

namespace coprocessor {

esp_err_t web_server_start() {
  if (g_server) {
    ESP_LOGI(TAG, "Web server already running");
    return ESP_OK;
  }

  httpd_config_t config = HTTPD_DEFAULT_CONFIG();
  config.server_port = 80;
  config.lru_purge_enable = true;
  config.max_uri_handlers = 8;
  config.max_open_sockets = 7;

  ESP_LOGI(TAG, "Starting web server on port %d...", config.server_port);
  esp_err_t ret = httpd_start(&g_server, &config);
  if (ret != ESP_OK) {
    ESP_LOGE(TAG, "Failed to start HTTP server: %d", ret);
    return ret;
  }

  httpd_uri_t index_uri = {
      .uri = "/",
      .method = HTTP_GET,
      .handler = index_get_handler,
      .user_ctx = nullptr,
      .is_websocket = false,
  };
  httpd_register_uri_handler(g_server, &index_uri);

  httpd_uri_t status_uri = {
      .uri = "/api/status",
      .method = HTTP_GET,
      .handler = status_get_handler,
      .user_ctx = nullptr,
      .is_websocket = false,
  };
  httpd_register_uri_handler(g_server, &status_uri);

  httpd_uri_t ws_uri = {
      .uri = "/ws",
      .method = HTTP_GET,
      .handler = ws_handler,
      .user_ctx = nullptr,
      .is_websocket = true,
#ifdef CONFIG_HTTPD_WS_POST_HANDSHAKE_CB_SUPPORT
      .ws_post_handshake_cb = ws_post_handshake_cb,
#endif
  };
  httpd_register_uri_handler(g_server, &ws_uri);

  httpd_uri_t notify_uri = {
      .uri = "/media/notify",
      .method = HTTP_NOTIFY,
      .handler = media_notify_handler,
      .user_ctx = nullptr,
      .is_websocket = false,
  };
  httpd_register_uri_handler(g_server, &notify_uri);

  ESP_LOGI(TAG,
           "Web server running: /, /api/status, /ws (WebSocket), /media/notify "
           "(GENA)");
  return ESP_OK;
}

esp_err_t web_server_stop() {
  if (!g_server) {
    return ESP_OK;
  }
  ESP_LOGI(TAG, "Stopping web server...");
  esp_err_t ret = httpd_stop(g_server);
  g_server = nullptr;
  return ret;
}

bool web_server_is_running() { return g_server != nullptr; }

void web_server_update_battery(const CoprocessorProto::BatteryStatus* status) {
  if (!status) {
    return;
  }
  g_battery_millivolts.store(status->millivolts());
  bool plugged = status->is_plugged();
  g_battery_plugged.store(plugged);
  if (plugged || status->percent() == 0xFF) {
    g_battery_percent.store(-1);
  } else {
    g_battery_percent.store(static_cast<int32_t>(status->percent()));
  }

  broadcast_status();
}

}  // namespace coprocessor
