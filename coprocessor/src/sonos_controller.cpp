// Copyright © 2026 Nilton Volpato
// SPDX-License-Identifier: MIT

#include "sonos_controller.h"

#include <arpa/inet.h>
#include <errno.h>
#include <strings.h>
#include <sys/socket.h>

#include <cinttypes>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <functional>
#include <optional>
#include <string>
#include <vector>

#include "esp_http_client.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "expat.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"
#include "uart_protocol.h"
#include "wifi_manager.h"

namespace coprocessor {

namespace {

constexpr const char* TAG = "sonos";

constexpr const char* URN_AVT = "urn:schemas-upnp-org:service:AVTransport:1";
constexpr const char* URN_RCS =
    "urn:schemas-upnp-org:service:RenderingControl:1";
constexpr const char* URN_ZGT =
    "urn:schemas-upnp-org:service:ZoneGroupTopology:1";

constexpr const char* PATH_AVT_CONTROL = "/MediaRenderer/AVTransport/Control";
constexpr const char* PATH_RCS_CONTROL =
    "/MediaRenderer/RenderingControl/Control";
constexpr const char* PATH_ZGT_CONTROL = "/ZoneGroupTopology/Control";

constexpr const char* PATH_AVT_EVENT = "/MediaRenderer/AVTransport/Event";
constexpr const char* PATH_RCS_EVENT = "/MediaRenderer/RenderingControl/Event";
constexpr const char* PATH_ZGT_EVENT = "/ZoneGroupTopology/Event";

constexpr uint32_t GENA_REQUESTED_TIMEOUT_SECONDS = 1800;
constexpr int64_t RENEW_RETRY_BACKOFF_MS = 30000;
constexpr size_t COMMAND_QUEUE_LENGTH = 16;
constexpr size_t MAX_ERROR_BODY_BYTES = 4096;

namespace Media = CoprocessorProto::Media;

// ---------------------------------------------------------------------------
// Command queue
// ---------------------------------------------------------------------------

enum class CommandType : uint8_t {
  Action,
  Volume,
  GetTopology,
  Subscribe,
  UnsubscribeAll,
  Notify,
};

struct SonosCommand {
  CommandType type;
  std::string ip;
  uint16_t port = 1400;
  Media::ActionType action = Media::ActionType_Play;
  int16_t volume = 0;
  bool is_relative = false;
  std::string sid;
  std::string body;
};

QueueHandle_t s_queue = nullptr;

bool post_command(SonosCommand* cmd) {
  if (!s_queue || xQueueSend(s_queue, &cmd, 0) != pdTRUE) {
    ESP_LOGW(TAG, "Command queue full, dropping command %d",
             static_cast<int>(cmd->type));
    delete cmd;
    return false;
  }
  return true;
}

// ---------------------------------------------------------------------------
// Expat plumbing
// ---------------------------------------------------------------------------

const char* local_name(const char* qname) {
  const char* colon = strchr(qname, ':');
  return colon ? colon + 1 : qname;
}

const char* find_attr(const XML_Char** atts, const char* name) {
  for (int i = 0; atts[i]; i += 2) {
    if (strcmp(atts[i], name) == 0) {
      return atts[i + 1];
    }
  }
  return nullptr;
}

// Pass 1 for SOAP responses: accumulate the text of one target element
// (character data arrives unescaped and possibly split across callbacks), and
// pick up SOAP fault details along the way.
struct SoapCapture {
  std::string capture_target;
  std::string captured;
  bool capturing = false;
  bool in_fault = false;
  std::string current_elem;
  std::string faultstring;
  std::string error_code_text;
};

void XMLCALL soap_capture_start(void* ud, const XML_Char* name,
                                const XML_Char** atts) {
  auto* s = static_cast<SoapCapture*>(ud);
  const char* ln = local_name(name);
  s->current_elem = ln;
  if (strcmp(ln, "Fault") == 0) {
    s->in_fault = true;
  }
  if (!s->capture_target.empty() && s->capture_target == ln) {
    s->capturing = true;
    s->captured.clear();
  }
}

void XMLCALL soap_capture_end(void* ud, const XML_Char* name) {
  auto* s = static_cast<SoapCapture*>(ud);
  const char* ln = local_name(name);
  if (s->capturing && !s->capture_target.empty() && s->capture_target == ln) {
    s->capturing = false;
  }
  if (strcmp(ln, "Fault") == 0) {
    s->in_fault = false;
  }
  s->current_elem.clear();
}

void XMLCALL soap_capture_chardata(void* ud, const XML_Char* str, int len) {
  auto* s = static_cast<SoapCapture*>(ud);
  if (s->capturing) {
    s->captured.append(str, len);
    return;
  }
  if (s->in_fault) {
    if (s->current_elem == "faultstring") {
      s->faultstring.append(str, len);
    } else if (s->current_elem == "errorCode") {
      s->error_code_text.append(str, len);
    }
  }
}

void soap_capture_parse(XML_Parser parser, SoapCapture* cap) {
  XML_SetUserData(parser, cap);
  XML_SetElementHandler(parser, soap_capture_start, soap_capture_end);
  XML_SetCharacterDataHandler(parser, soap_capture_chardata);
}

bool soap_capture_parse_string(const std::string& xml, SoapCapture* cap) {
  XML_Parser parser = XML_ParserCreate(nullptr);
  if (!parser) {
    return false;
  }
  soap_capture_parse(parser, cap);
  bool ok =
      XML_Parse(parser, xml.data(), xml.size(), XML_TRUE) != XML_STATUS_ERROR;
  if (!ok) {
    ESP_LOGW(TAG, "XML parse error: %s",
             XML_ErrorString(XML_GetErrorCode(parser)));
  }
  XML_ParserFree(parser);
  return ok;
}

// Pass 2 for GetZoneGroupState: all topology data lives in attributes.
struct ZgsMember {
  std::string uuid;
  std::string name;
  std::string ip;
  uint16_t port = 1400;
  bool invisible = false;
};

struct ZgsGroup {
  std::string id;
  std::string coordinator_uuid;
  std::vector<ZgsMember> members;
};

struct ZgsParse {
  std::vector<ZgsGroup> groups;
  ZgsGroup current;
  bool in_group = false;
};

void parse_location(const std::string& location, std::string* ip,
                    uint16_t* port) {
  size_t host_start = 0;
  size_t scheme = location.find("://");
  if (scheme != std::string::npos) {
    host_start = scheme + 3;
  }
  size_t colon = location.find(':', host_start);
  size_t slash = location.find('/', host_start);
  size_t host_end = location.size();
  if (colon != std::string::npos && colon < host_end) {
    host_end = colon;
  }
  if (slash != std::string::npos && slash < host_end) {
    host_end = slash;
  }
  *ip = location.substr(host_start, host_end - host_start);
  if (colon != std::string::npos &&
      (slash == std::string::npos || colon < slash)) {
    int p = atoi(location.c_str() + colon + 1);
    if (p > 0 && p <= 65535) {
      *port = static_cast<uint16_t>(p);
    }
  }
}

void XMLCALL zgs_start(void* ud, const XML_Char* name, const XML_Char** atts) {
  auto* s = static_cast<ZgsParse*>(ud);
  const char* ln = local_name(name);
  if (strcmp(ln, "ZoneGroup") == 0) {
    s->in_group = true;
    s->current = ZgsGroup{};
    const char* v;
    if ((v = find_attr(atts, "Coordinator"))) {
      s->current.coordinator_uuid = v;
    }
    if ((v = find_attr(atts, "ID"))) {
      s->current.id = v;
    }
  } else if (s->in_group && strcmp(ln, "ZoneGroupMember") == 0) {
    ZgsMember m;
    std::string location;
    const char* v;
    if ((v = find_attr(atts, "UUID"))) {
      m.uuid = v;
    }
    if ((v = find_attr(atts, "ZoneName"))) {
      m.name = v;
    }
    if ((v = find_attr(atts, "Location"))) {
      location = v;
    }
    if ((v = find_attr(atts, "Invisible"))) {
      m.invisible = strcmp(v, "1") == 0;
    }
    if (!location.empty()) {
      parse_location(location, &m.ip, &m.port);
    }
    if (!m.ip.empty()) {
      s->current.members.push_back(std::move(m));
    }
  }
}

void XMLCALL zgs_end(void* ud, const XML_Char* name) {
  auto* s = static_cast<ZgsParse*>(ud);
  if (strcmp(local_name(name), "ZoneGroup") != 0) {
    return;
  }
  s->in_group = false;
  if (!s->current.members.empty()) {  // skip empty groups
    s->groups.push_back(std::move(s->current));
  }
  s->current = ZgsGroup{};
}

bool parse_zone_group_state(const std::string& xml, ZgsParse* out) {
  XML_Parser parser = XML_ParserCreate(nullptr);
  if (!parser) {
    return false;
  }
  XML_SetUserData(parser, out);
  XML_SetElementHandler(parser, zgs_start, zgs_end);
  bool ok =
      XML_Parse(parser, xml.data(), xml.size(), XML_TRUE) != XML_STATUS_ERROR;
  if (!ok) {
    ESP_LOGW(TAG, "ZoneGroupState parse error: %s",
             XML_ErrorString(XML_GetErrorCode(parser)));
  }
  XML_ParserFree(parser);
  return ok;
}

// Pass 1 for GENA NOTIFY bodies: collect every <LastChange> text blob and
// detect a directly embedded <ZoneGroupState> (means regrouping happened).
struct NotifyParse {
  std::vector<std::string> last_changes;
  std::string current_lc;
  bool in_lastchange = false;
  bool zone_group_state = false;
};

void XMLCALL notify_start(void* ud, const XML_Char* name,
                          const XML_Char** atts) {
  auto* s = static_cast<NotifyParse*>(ud);
  const char* ln = local_name(name);
  if (strcmp(ln, "LastChange") == 0) {
    s->in_lastchange = true;
    s->current_lc.clear();
  } else if (strcmp(ln, "ZoneGroupState") == 0) {
    s->zone_group_state = true;
  }
}

void XMLCALL notify_end(void* ud, const XML_Char* name) {
  auto* s = static_cast<NotifyParse*>(ud);
  if (strcmp(local_name(name), "LastChange") == 0 && s->in_lastchange) {
    s->in_lastchange = false;
    if (!s->current_lc.empty()) {
      s->last_changes.push_back(std::move(s->current_lc));
      s->current_lc.clear();
    }
  }
}

void XMLCALL notify_chardata(void* ud, const XML_Char* str, int len) {
  auto* s = static_cast<NotifyParse*>(ud);
  if (s->in_lastchange) {
    s->current_lc.append(str, len);
  }
}

bool parse_notify_body(const std::string& xml, NotifyParse* out) {
  XML_Parser parser = XML_ParserCreate(nullptr);
  if (!parser) {
    return false;
  }
  XML_SetUserData(parser, out);
  XML_SetElementHandler(parser, notify_start, notify_end);
  XML_SetCharacterDataHandler(parser, notify_chardata);
  bool ok =
      XML_Parse(parser, xml.data(), xml.size(), XML_TRUE) != XML_STATUS_ERROR;
  if (!ok) {
    ESP_LOGW(TAG, "NOTIFY parse error: %s",
             XML_ErrorString(XML_GetErrorCode(parser)));
  }
  XML_ParserFree(parser);
  return ok;
}

// Pass 2 for LastChange: every value sits in a `val` attribute.
struct LastChangeParse {
  std::optional<Media::TransportState> transport;
  std::optional<uint8_t> volume;
  std::optional<bool> muted;
  bool has_track = false;
  std::string didl;
  std::optional<uint32_t> duration_seconds;
  std::optional<uint32_t> elapsed_seconds;
};

std::optional<Media::TransportState> parse_transport_state(const char* v) {
  if (strcmp(v, "PLAYING") == 0) {
    return Media::TransportState_Playing;
  }
  if (strcmp(v, "PAUSED_PLAYBACK") == 0) {
    return Media::TransportState_Paused;
  }
  if (strcmp(v, "STOPPED") == 0) {
    return Media::TransportState_Stopped;
  }
  if (strcmp(v, "TRANSITIONING") == 0) {
    return Media::TransportState_Transitioning;
  }
  return std::nullopt;
}

// Parses "H:MM:SS"; returns nullopt for anything else (incl. NOT_IMPLEMENTED).
std::optional<uint32_t> parse_hms(const char* v) {
  // A leading '-' would wrap to a huge value via %u.
  if (!v || v[0] == '-' || strcasecmp(v, "NOT_IMPLEMENTED") == 0) {
    return std::nullopt;
  }
  unsigned h = 0, m = 0, sec = 0;
  if (sscanf(v, "%u:%u:%u", &h, &m, &sec) == 3) {
    return h * 3600 + m * 60 + sec;
  }
  return std::nullopt;
}

bool channel_is_master(const char* channel) {
  return !channel || strcmp(channel, "Master") == 0;
}

void XMLCALL last_change_start(void* ud, const XML_Char* name,
                               const XML_Char** atts) {
  auto* s = static_cast<LastChangeParse*>(ud);
  const char* ln = local_name(name);
  const char* val = find_attr(atts, "val");
  if (!val) {
    return;
  }
  if (strcmp(ln, "TransportState") == 0) {
    s->transport = parse_transport_state(val);
  } else if (strcmp(ln, "Volume") == 0) {
    if (channel_is_master(find_attr(atts, "channel"))) {
      int v = atoi(val);
      if (v < 0) {
        v = 0;
      }
      if (v > 100) {
        v = 100;
      }
      s->volume = static_cast<uint8_t>(v);
    }
  } else if (strcmp(ln, "Mute") == 0) {
    if (channel_is_master(find_attr(atts, "channel"))) {
      s->muted = strcmp(val, "1") == 0 || strcasecmp(val, "true") == 0;
    }
  } else if (strcmp(ln, "CurrentTrackMetaData") == 0) {
    s->has_track = true;
    if (val[0] != '\0' && strcasecmp(val, "NOT_IMPLEMENTED") != 0) {
      s->didl = val;
    }
  } else if (strcmp(ln, "CurrentTrackDuration") == 0) {
    s->duration_seconds = parse_hms(val);
  } else if (strcmp(ln, "RelativeTimePosition") == 0) {
    s->elapsed_seconds = parse_hms(val);
  }
}

bool parse_last_change(const std::string& xml, LastChangeParse* out) {
  XML_Parser parser = XML_ParserCreate(nullptr);
  if (!parser) {
    return false;
  }
  XML_SetUserData(parser, out);
  XML_SetElementHandler(parser, last_change_start, nullptr);
  bool ok =
      XML_Parse(parser, xml.data(), xml.size(), XML_TRUE) != XML_STATUS_ERROR;
  if (!ok) {
    ESP_LOGW(TAG, "LastChange parse error: %s",
             XML_ErrorString(XML_GetErrorCode(parser)));
  }
  XML_ParserFree(parser);
  return ok;
}

// Pass 3 for CurrentTrackMetaData: DIDL-Lite, element text matched by local
// name (dc:title, dc:creator, upnp:album).
struct DidlParse {
  std::string title;
  std::string artist;
  std::string album;
  enum class Capturing : uint8_t {
    None,
    Title,
    Artist,
    Album
  } capturing = Capturing::None;
  bool saw_title = false;
  bool saw_artist = false;
  bool saw_album = false;
};

void XMLCALL didl_start(void* ud, const XML_Char* name, const XML_Char** atts) {
  auto* s = static_cast<DidlParse*>(ud);
  const char* ln = local_name(name);
  if (strcmp(ln, "title") == 0 && !s->saw_title) {
    s->capturing = DidlParse::Capturing::Title;
    s->saw_title = true;
  } else if ((strcmp(ln, "creator") == 0 || strcmp(ln, "artist") == 0) &&
             !s->saw_artist) {
    s->capturing = DidlParse::Capturing::Artist;
    s->saw_artist = true;
  } else if (strcmp(ln, "album") == 0 && !s->saw_album) {
    s->capturing = DidlParse::Capturing::Album;
    s->saw_album = true;
  }
}

void XMLCALL didl_end(void* ud, const XML_Char* name) {
  auto* s = static_cast<DidlParse*>(ud);
  s->capturing = DidlParse::Capturing::None;
}

void XMLCALL didl_chardata(void* ud, const XML_Char* str, int len) {
  auto* s = static_cast<DidlParse*>(ud);
  switch (s->capturing) {
    case DidlParse::Capturing::Title:
      s->title.append(str, len);
      break;
    case DidlParse::Capturing::Artist:
      s->artist.append(str, len);
      break;
    case DidlParse::Capturing::Album:
      s->album.append(str, len);
      break;
    default:
      break;
  }
}

bool parse_didl(const std::string& xml, DidlParse* out) {
  XML_Parser parser = XML_ParserCreate(nullptr);
  if (!parser) {
    return false;
  }
  XML_SetUserData(parser, out);
  XML_SetElementHandler(parser, didl_start, didl_end);
  XML_SetCharacterDataHandler(parser, didl_chardata);
  bool ok =
      XML_Parse(parser, xml.data(), xml.size(), XML_TRUE) != XML_STATUS_ERROR;
  if (!ok) {
    ESP_LOGW(TAG, "DIDL-Lite parse error: %s",
             XML_ErrorString(XML_GetErrorCode(parser)));
  }
  XML_ParserFree(parser);
  return ok;
}

// ---------------------------------------------------------------------------
// SOAP over esp_http_client
// ---------------------------------------------------------------------------

std::string build_soap_envelope(const char* urn, const char* action,
                                const std::string& args) {
  std::string body;
  body.reserve(320 + args.size());
  body +=
      "<s:Envelope "
      "xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" "
      "s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><"
      "u:";
  body += action;
  body += " xmlns:u=\"";
  body += urn;
  body += "\">";
  body += args;
  body += "</u:";
  body += action;
  body += "></s:Body></s:Envelope>";
  return body;
}

// POSTs a SOAP action and streams the response body chunks to `on_body`
// (HTTP 200 only). Returns true on HTTP 200 with a cleanly read body; a read
// error mid-body counts as failure. HTTP 500 SOAP faults are parsed and
// logged; UPnP error codes 701/711 (illegal for current source) are benign
// and logged at info level.
bool soap_request(const std::string& ip, uint16_t port,
                  const char* control_path, const char* service_urn,
                  const char* action, const std::string& args,
                  const std::function<bool(const char*, int)>& on_body) {
  std::string url = "http://" + ip + ":" + std::to_string(port) + control_path;
  std::string body = build_soap_envelope(service_urn, action, args);
  std::string soap_action =
      std::string("\"") + service_urn + "#" + action + "\"";

  esp_http_client_config_t cfg = {};
  cfg.url = url.c_str();
  cfg.method = HTTP_METHOD_POST;
  cfg.timeout_ms = 5000;
  cfg.buffer_size = 512;

  esp_http_client_handle_t client = esp_http_client_init(&cfg);
  if (!client) {
    ESP_LOGE(TAG, "SOAP %s: esp_http_client_init failed", action);
    return false;
  }
  esp_http_client_set_header(client, "Content-Type",
                             "text/xml; charset=\"utf-8\"");
  esp_http_client_set_header(client, "SOAPAction", soap_action.c_str());

  bool success = false;
  esp_err_t err = esp_http_client_open(client, body.size());
  if (err != ESP_OK) {
    ESP_LOGW(TAG, "SOAP %s to %s: connect failed: %s", action, url.c_str(),
             esp_err_to_name(err));
    esp_http_client_cleanup(client);
    return false;
  }
  if (esp_http_client_write(client, body.data(), body.size()) < 0) {
    ESP_LOGW(TAG, "SOAP %s: write failed", action);
    esp_http_client_cleanup(client);
    return false;
  }
  esp_http_client_fetch_headers(client);
  int status = esp_http_client_get_status_code(client);

  std::string err_body;
  bool read_error = false;
  char buf[256];
  while (true) {
    int n = esp_http_client_read(client, buf, sizeof(buf));
    if (n < 0) {
      ESP_LOGW(TAG, "SOAP %s: read error mid-body", action);
      read_error = true;
      break;
    }
    if (n == 0) {
      break;
    }
    if (status == 200) {
      if (on_body && !on_body(buf, n)) {
        break;
      }
    } else if (err_body.size() < MAX_ERROR_BODY_BYTES) {
      // Keep only the head of the error body for fault parsing.
      size_t room = MAX_ERROR_BODY_BYTES - err_body.size();
      err_body.append(
          buf, static_cast<size_t>(n) < room ? static_cast<size_t>(n) : room);
    }
  }
  esp_http_client_cleanup(client);

  if (status == 200 && !read_error) {
    success = true;
  } else if (status == 200) {
    ESP_LOGW(TAG, "SOAP %s: truncated body", action);
  } else if (status == 500) {
    SoapCapture cap;  // no capture target: fault fields only
    soap_capture_parse_string(err_body, &cap);
    int error_code = atoi(cap.error_code_text.c_str());
    if (error_code == 701 || error_code == 711) {
      ESP_LOGI(TAG, "SOAP %s: benign UPnP fault %d (%s)", action, error_code,
               cap.faultstring.c_str());
    } else {
      ESP_LOGW(TAG, "SOAP %s: HTTP 500 UPnP fault %d: %s", action, error_code,
               cap.faultstring.c_str());
    }
  } else {
    ESP_LOGW(TAG, "SOAP %s: unexpected HTTP %d", action, status);
  }
  return success;
}

// ---------------------------------------------------------------------------
// GENA over raw sockets (esp_http_client has no SUBSCRIBE/UNSUBSCRIBE method)
// ---------------------------------------------------------------------------

struct GenaResponse {
  int status_code = 0;
  std::string sid;
  uint32_t timeout_seconds = 0;
};

bool header_name_eq(const std::string& a, const char* b) {
  return strcasecmp(a.c_str(), b) == 0;
}

// Sends a raw HTTP/1.1 request (SUBSCRIBE/UNSUBSCRIBE), reads the response
// headers, and extracts status code, SID and TIMEOUT.
bool gena_request(const std::string& ip, uint16_t port,
                  const std::string& request, GenaResponse* resp) {
  struct sockaddr_in addr = {};
  addr.sin_family = AF_INET;
  addr.sin_port = htons(port);
  if (inet_pton(AF_INET, ip.c_str(), &addr.sin_addr) != 1) {
    ESP_LOGW(TAG, "GENA: malformed IPv4 address '%s'", ip.c_str());
    return false;
  }

  int fd = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
  if (fd < 0) {
    ESP_LOGW(TAG, "GENA: socket() failed (errno=%d)", errno);
    return false;
  }
  struct timeval tv = {};
  tv.tv_sec = 5;
  setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
  setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv));

  bool ok = false;
  if (connect(fd, reinterpret_cast<struct sockaddr*>(&addr), sizeof(addr)) !=
      0) {
    ESP_LOGW(TAG, "GENA: connect to %s:%u failed (errno=%d)", ip.c_str(), port,
             errno);
    close(fd);
    return false;
  }
  size_t sent = 0;
  while (sent < request.size()) {
    int n = send(fd, request.data() + sent, request.size() - sent, 0);
    if (n <= 0) {
      ESP_LOGW(TAG, "GENA: send failed (errno=%d)", errno);
      close(fd);
      return false;
    }
    sent += n;
  }

  std::string raw;
  char buf[256];
  while (raw.find("\r\n\r\n") == std::string::npos) {
    int n = recv(fd, buf, sizeof(buf), 0);
    if (n <= 0) {
      break;
    }
    raw.append(buf, n);
    if (raw.size() > 4096) {
      break;
    }
  }
  close(fd);

  size_t header_end = raw.find("\r\n\r\n");
  if (header_end == std::string::npos) {
    ESP_LOGW(TAG, "GENA: no complete response headers from %s", ip.c_str());
    return false;
  }
  size_t sp = raw.find(' ');
  if (sp == std::string::npos) {
    return false;
  }
  resp->status_code = atoi(raw.c_str() + sp + 1);

  size_t pos = raw.find("\r\n");
  while (pos != std::string::npos && pos + 2 < header_end) {
    size_t line_start = pos + 2;
    size_t line_end = raw.find("\r\n", line_start);
    if (line_end == std::string::npos || line_end > header_end) {
      line_end = header_end;
    }
    std::string line = raw.substr(line_start, line_end - line_start);
    size_t colon = line.find(':');
    if (colon != std::string::npos) {
      std::string name = line.substr(0, colon);
      std::string value = line.substr(colon + 1);
      while (!value.empty() &&
             (value.front() == ' ' || value.front() == '\t')) {
        value.erase(value.begin());
      }
      while (!value.empty() && (value.back() == ' ' || value.back() == '\t' ||
                                value.back() == '\r' || value.back() == '\n')) {
        value.pop_back();
      }
      while (!name.empty() && name.back() == ' ') {
        name.pop_back();
      }
      if (header_name_eq(name, "SID")) {
        resp->sid = value;
      } else if (header_name_eq(name, "TIMEOUT")) {
        if (strncasecmp(value.c_str(), "Second-", 7) == 0) {
          // "Second-infinite" yields 0 here; gena_subscribe deliberately
          // falls back to the requested 1800s in that case.
          resp->timeout_seconds =
              static_cast<uint32_t>(atoi(value.c_str() + 7));
        }
      }
    }
    pos = line_end;
  }
  ok = true;
  return ok;
}

struct Subscription {
  std::string event_url;
  std::string sid;
  uint32_t granted_seconds = 0;
  int64_t renew_at_ms = 0;
};

std::vector<Subscription> s_subscriptions;
std::string s_coordinator_ip;
uint16_t s_coordinator_port = 1400;

enum class GenaRequestKind : uint8_t { Subscribe, Renew, Unsubscribe };

std::string build_gena_request(GenaRequestKind kind, const std::string& ip,
                               uint16_t port, const std::string& event_url,
                               const std::string& sid) {
  std::string req =
      kind == GenaRequestKind::Unsubscribe ? "UNSUBSCRIBE " : "SUBSCRIBE ";
  req += event_url + " HTTP/1.1\r\n";
  req += "HOST: " + ip + ":" + std::to_string(port) + "\r\n";
  if (kind == GenaRequestKind::Subscribe) {
    req += "CALLBACK: <http://";
    req += wifi_get_ip();
    req += ":80/media/notify>\r\n";
    req += "NT: upnp:event\r\n";
  } else if (!sid.empty()) {
    req += "SID: " + sid + "\r\n";
  }
  if (kind != GenaRequestKind::Unsubscribe) {
    req += "TIMEOUT: Second-" + std::to_string(GENA_REQUESTED_TIMEOUT_SECONDS) +
           "\r\n";
  }
  req += "CONNECTION: close\r\n\r\n";
  return req;
}

// Returns true and fills `sub` on HTTP 200 with a SID.
bool gena_subscribe(const std::string& ip, uint16_t port, const char* event_url,
                    Subscription* sub) {
  std::string req =
      build_gena_request(GenaRequestKind::Subscribe, ip, port, event_url, "");
  GenaResponse resp;
  if (!gena_request(ip, port, req, &resp)) {
    return false;
  }
  if (resp.status_code != 200 || resp.sid.empty()) {
    ESP_LOGW(TAG, "SUBSCRIBE %s failed: HTTP %d", event_url, resp.status_code);
    return false;
  }
  sub->event_url = event_url;
  sub->sid = resp.sid;
  sub->granted_seconds = resp.timeout_seconds > 0
                             ? resp.timeout_seconds
                             : GENA_REQUESTED_TIMEOUT_SECONDS;
  sub->renew_at_ms =
      esp_timer_get_time() / 1000 + sub->granted_seconds * 1000 / 2;
  return true;
}

// Renewal SUBSCRIBE carries only SID + TIMEOUT (no CALLBACK, no NT).
// Returns 0 on success, 1 on HTTP 412 (SID gone, needs full re-subscribe),
// 2 on any other failure.
int gena_renew(const std::string& ip, uint16_t port, Subscription* sub) {
  std::string req = build_gena_request(GenaRequestKind::Renew, ip, port,
                                       sub->event_url, sub->sid);
  GenaResponse resp;
  if (!gena_request(ip, port, req, &resp)) {
    return 2;
  }
  if (resp.status_code == 200) {
    if (resp.timeout_seconds > 0) {
      sub->granted_seconds = resp.timeout_seconds;
    }
    sub->renew_at_ms =
        esp_timer_get_time() / 1000 + sub->granted_seconds * 1000 / 2;
    return 0;
  }
  return resp.status_code == 412 ? 1 : 2;
}

void gena_unsubscribe(const std::string& ip, uint16_t port,
                      const Subscription& sub) {
  std::string req = build_gena_request(GenaRequestKind::Unsubscribe, ip, port,
                                       sub.event_url, sub.sid);
  GenaResponse resp;
  if (gena_request(ip, port, req, &resp) && resp.status_code != 200) {
    ESP_LOGW(TAG, "UNSUBSCRIBE %s: HTTP %d", sub.event_url.c_str(),
             resp.status_code);
  }
}

void unsubscribe_all_internal() {
  if (s_subscriptions.empty()) {
    return;
  }
  if (wifi_is_connected() && !s_coordinator_ip.empty()) {
    for (const auto& sub : s_subscriptions) {
      gena_unsubscribe(s_coordinator_ip, s_coordinator_port, sub);
    }
  }
  ESP_LOGI(TAG, "Dropped %zu subscription(s) for %s", s_subscriptions.size(),
           s_coordinator_ip.c_str());
  s_subscriptions.clear();
  s_coordinator_ip.clear();
}

// ---------------------------------------------------------------------------
// Command handlers (run on the Sonos task)
// ---------------------------------------------------------------------------

bool query_transport_state(const std::string& ip, uint16_t port,
                           Media::TransportState* out) {
  std::string body;
  bool ok =
      soap_request(ip, port, PATH_AVT_CONTROL, URN_AVT, "GetTransportInfo",
                   "<InstanceID>0</InstanceID>", [&body](const char* d, int n) {
                     body.append(d, n);
                     return true;
                   });
  if (!ok) {
    return false;
  }
  SoapCapture cap;
  cap.capture_target = "CurrentTransportState";
  if (!soap_capture_parse_string(body, &cap) || cap.captured.empty()) {
    return false;
  }
  auto state = parse_transport_state(cap.captured.c_str());
  if (!state) {
    return false;
  }
  *out = *state;
  return true;
}

const char* action_name_for(Media::ActionType action) {
  switch (action) {
    case Media::ActionType_Play:
      return "Play";
    case Media::ActionType_Pause:
      return "Pause";
    case Media::ActionType_Next:
      return "Next";
    case Media::ActionType_Previous:
      return "Previous";
    case Media::ActionType_Stop:
      return "Stop";
    default:
      return nullptr;
  }
}

void do_action(const SonosCommand& cmd) {
  Media::ActionType action = cmd.action;
  if (action == Media::ActionType_TogglePlayPause) {
    Media::TransportState state;
    if (query_transport_state(cmd.ip, cmd.port, &state)) {
      action = state == Media::TransportState_Playing ? Media::ActionType_Pause
                                                      : Media::ActionType_Play;
    } else {
      ESP_LOGW(TAG,
               "TogglePlayPause: GetTransportInfo failed, defaulting to Play");
      action = Media::ActionType_Play;
    }
  }
  const char* name = action_name_for(action);
  if (!name) {
    return;
  }
  std::string args = "<InstanceID>0</InstanceID>";
  if (action == Media::ActionType_Play) {
    args += "<Speed>1</Speed>";
  }
  ESP_LOGI(TAG, "Action %s -> %s:%u", name, cmd.ip.c_str(), cmd.port);
  soap_request(cmd.ip, cmd.port, PATH_AVT_CONTROL, URN_AVT, name, args, {});
}

void do_volume(const SonosCommand& cmd) {
  char args[128];
  const char* action;
  if (cmd.is_relative) {
    snprintf(args, sizeof(args),
             "<InstanceID>0</InstanceID><Channel>Master</Channel><Adjustment>%"
             "d</Adjustment>",
             static_cast<int>(cmd.volume));
    action = "SetRelativeVolume";
  } else {
    int v = cmd.volume;
    if (v < 0) {
      v = 0;
    }
    if (v > 100) {
      v = 100;
    }
    snprintf(
        args, sizeof(args),
        "<InstanceID>0</InstanceID><Channel>Master</Channel><DesiredVolume>"
        "%d</DesiredVolume>",
        v);
    action = "SetVolume";
  }
  ESP_LOGI(TAG, "Volume %s (%d) -> %s:%u", action, static_cast<int>(cmd.volume),
           cmd.ip.c_str(), cmd.port);
  soap_request(cmd.ip, cmd.port, PATH_RCS_CONTROL, URN_RCS, action, args, {});
}

void fetch_and_send_topology(const std::string& ip, uint16_t port) {
  SoapCapture cap;
  cap.capture_target = "ZoneGroupState";
  XML_Parser parser = XML_ParserCreate(nullptr);
  if (!parser) {
    return;
  }
  soap_capture_parse(parser, &cap);
  bool stream_ok = true;
  bool ok = soap_request(
      ip, port, PATH_ZGT_CONTROL, URN_ZGT, "GetZoneGroupState", "",
      [parser, &stream_ok](const char* d, int n) {
        if (XML_Parse(parser, d, n, XML_FALSE) == XML_STATUS_ERROR) {
          stream_ok = false;
          return false;
        }
        return true;
      });
  if (XML_Parse(parser, "", 0, XML_TRUE) == XML_STATUS_ERROR) {
    stream_ok = false;
  }
  XML_ParserFree(parser);
  if (!ok || !stream_ok || cap.captured.empty()) {
    ESP_LOGW(TAG, "GetZoneGroupState from %s:%u failed", ip.c_str(), port);
    return;
  }

  ZgsParse zgs;
  if (!parse_zone_group_state(cap.captured, &zgs)) {
    return;
  }

  std::vector<UartMediaGroup> groups;
  for (const auto& g : zgs.groups) {
    UartMediaGroup out;
    out.id = g.id;
    const ZgsMember* coordinator = nullptr;
    const ZgsMember* first_visible = nullptr;
    for (const auto& m : g.members) {
      if (m.uuid == g.coordinator_uuid) {
        coordinator = &m;
      }
      if (!m.invisible && !first_visible) {
        first_visible = &m;
      }
    }
    if (!coordinator) {
      ESP_LOGW(TAG,
               "Group '%s': coordinator UUID %s not found among members, "
               "falling back to first visible member",
               g.id.c_str(), g.coordinator_uuid.c_str());
    }
    const ZgsMember* name_src = coordinator ? coordinator : first_visible;
    out.name = name_src ? name_src->name : "Unknown";
    const ZgsMember* coord_src = name_src ? name_src : &g.members.front();
    out.coordinator_ip = coord_src->ip;
    out.coordinator_port = coord_src->port;
    for (const auto& m : g.members) {
      out.members.push_back(UartMediaMember{m.name, m.uuid, m.ip});
    }
    groups.push_back(std::move(out));
  }
  ESP_LOGI(TAG, "Topology: %zu group(s) from %s", groups.size(), ip.c_str());
  uart_send_media_topology_update(groups);
}

void do_subscribe(const std::string& ip, uint16_t port) {
  unsubscribe_all_internal();

  static const char* const kEventUrls[] = {PATH_AVT_EVENT, PATH_RCS_EVENT,
                                           PATH_ZGT_EVENT};
  for (const char* url : kEventUrls) {
    Subscription sub;
    if (gena_subscribe(ip, port, url, &sub)) {
      ESP_LOGI(TAG, "Subscribed to %s:%u%s (SID=%s, timeout=%" PRIu32 "s)",
               ip.c_str(), port, url, sub.sid.c_str(), sub.granted_seconds);
      s_subscriptions.push_back(std::move(sub));
    } else {
      ESP_LOGW(TAG, "Failed to subscribe to %s:%u%s", ip.c_str(), port, url);
    }
  }
  if (!s_subscriptions.empty()) {
    s_coordinator_ip = ip;
    s_coordinator_port = port;
  }
}

void do_notify(const SonosCommand& cmd) {
  // Only accept events for currently active subscriptions; late NOTIFYs from
  // a stale subscription (e.g. after a group switch) must not be attributed
  // to the new coordinator.
  bool sid_known = false;
  for (const auto& sub : s_subscriptions) {
    if (sub.sid == cmd.sid) {
      sid_known = true;
      break;
    }
  }
  if (!sid_known) {
    ESP_LOGD(TAG, "NOTIFY with unknown SID '%s', dropping", cmd.sid.c_str());
    return;
  }

  NotifyParse np;
  if (!parse_notify_body(cmd.body, &np)) {
    return;
  }

  // A directly embedded ZoneGroupState property means the household
  // regrouped: refresh the whole topology from the subscribed coordinator.
  if (np.zone_group_state && !s_coordinator_ip.empty()) {
    ESP_LOGI(TAG, "ZoneGroupState changed, refreshing topology");
    fetch_and_send_topology(s_coordinator_ip, s_coordinator_port);
  }

  UartMediaStateUpdate update;
  update.endpoint_ip = s_coordinator_ip;
  bool any_field = false;
  for (const auto& lc : np.last_changes) {
    LastChangeParse lcp;
    if (!parse_last_change(lc, &lcp)) {
      continue;
    }
    if (lcp.transport) {
      update.transport_state = lcp.transport;
      any_field = true;
    }
    if (lcp.volume) {
      update.volume = lcp.volume;
      any_field = true;
    }
    if (lcp.muted) {
      update.is_muted = lcp.muted;
      any_field = true;
    }
    UartMediaTrackMetadata track;
    bool has_track_fields = false;
    if (lcp.has_track) {
      has_track_fields = true;
      if (!lcp.didl.empty()) {
        DidlParse didl;
        if (parse_didl(lcp.didl, &didl)) {
          if (!didl.title.empty()) {
            track.title = didl.title;
          }
          if (!didl.artist.empty()) {
            track.artist = didl.artist;
          }
          if (!didl.album.empty()) {
            track.album = didl.album;
          }
        }
      }
    }
    if (lcp.duration_seconds) {
      track.duration_seconds = lcp.duration_seconds;
      has_track_fields = true;
    }
    if (lcp.elapsed_seconds) {
      track.elapsed_seconds = lcp.elapsed_seconds;
      has_track_fields = true;
    }
    if (has_track_fields) {
      update.track = std::move(track);
      any_field = true;
    }
  }

  if (any_field && !update.endpoint_ip.empty()) {
    uart_send_media_state_update(update);
  }
}

void check_subscriptions() {
  if (s_subscriptions.empty()) {
    return;
  }
  if (!wifi_is_connected()) {
    // Sockets are dead anyway; drop everything. A fresh explicit Subscribe
    // command will re-subscribe.
    ESP_LOGW(TAG, "Wi-Fi disconnected, dropping %zu subscription(s)",
             s_subscriptions.size());
    s_subscriptions.clear();
    s_coordinator_ip.clear();
    return;
  }
  int64_t now_ms = esp_timer_get_time() / 1000;
  for (auto& sub : s_subscriptions) {
    if (now_ms < sub.renew_at_ms) {
      continue;
    }
    int result = gena_renew(s_coordinator_ip, s_coordinator_port, &sub);
    if (result == 0) {
      ESP_LOGI(TAG, "Renewed %s (timeout=%" PRIu32 "s)", sub.event_url.c_str(),
               sub.granted_seconds);
    } else if (result == 1) {
      ESP_LOGW(TAG, "Renewal of %s got HTTP 412, re-subscribing",
               sub.event_url.c_str());
      Subscription fresh;
      if (gena_subscribe(s_coordinator_ip, s_coordinator_port,
                         sub.event_url.c_str(), &fresh)) {
        sub = std::move(fresh);
      } else {
        sub.renew_at_ms = now_ms + RENEW_RETRY_BACKOFF_MS;
      }
    } else {
      ESP_LOGW(TAG, "Renewal of %s failed, retry in %" PRId64 " ms",
               sub.event_url.c_str(), RENEW_RETRY_BACKOFF_MS);
      sub.renew_at_ms = now_ms + RENEW_RETRY_BACKOFF_MS;
    }
  }
}

void process_command(SonosCommand& cmd) {
  if (cmd.type != CommandType::UnsubscribeAll && !wifi_is_connected()) {
    ESP_LOGW(TAG, "Dropping command %d: Wi-Fi not connected",
             static_cast<int>(cmd.type));
    return;
  }
  switch (cmd.type) {
    case CommandType::Action:
      do_action(cmd);
      break;
    case CommandType::Volume:
      do_volume(cmd);
      break;
    case CommandType::GetTopology:
      fetch_and_send_topology(cmd.ip, cmd.port);
      break;
    case CommandType::Subscribe:
      do_subscribe(cmd.ip, cmd.port);
      break;
    case CommandType::UnsubscribeAll:
      unsubscribe_all_internal();
      break;
    case CommandType::Notify:
      do_notify(cmd);
      break;
  }
}

void sonos_task(void* pvParameters) {
  ESP_LOGI(TAG, "Sonos controller task started");
  while (true) {
    SonosCommand* cmd = nullptr;
    if (xQueueReceive(s_queue, &cmd, pdMS_TO_TICKS(5000)) == pdTRUE && cmd) {
      process_command(*cmd);
      delete cmd;
    }
    check_subscriptions();
  }
}

}  // namespace

void sonos_controller_init() {
  if (s_queue) {
    return;
  }
  s_queue = xQueueCreate(COMMAND_QUEUE_LENGTH, sizeof(SonosCommand*));
  configASSERT(s_queue);
  xTaskCreatePinnedToCore(sonos_task, "sonos_task", 8192, nullptr, 4, nullptr,
                          1);
}

bool sonos_post_action(const char* ip, uint16_t port,
                       Media::ActionType action) {
  auto* cmd = new SonosCommand{};
  cmd->type = CommandType::Action;
  cmd->ip = ip;
  cmd->port = port;
  cmd->action = action;
  return post_command(cmd);
}

bool sonos_post_volume(const char* ip, uint16_t port, int16_t volume,
                       bool is_relative) {
  auto* cmd = new SonosCommand{};
  cmd->type = CommandType::Volume;
  cmd->ip = ip;
  cmd->port = port;
  cmd->volume = volume;
  cmd->is_relative = is_relative;
  return post_command(cmd);
}

bool sonos_post_get_topology(const char* ip, uint16_t port) {
  auto* cmd = new SonosCommand{};
  cmd->type = CommandType::GetTopology;
  cmd->ip = ip;
  cmd->port = port;
  return post_command(cmd);
}

bool sonos_post_subscribe(const char* ip, uint16_t port) {
  auto* cmd = new SonosCommand{};
  cmd->type = CommandType::Subscribe;
  cmd->ip = ip;
  cmd->port = port;
  return post_command(cmd);
}

bool sonos_post_unsubscribe_all() {
  auto* cmd = new SonosCommand{};
  cmd->type = CommandType::UnsubscribeAll;
  return post_command(cmd);
}

bool sonos_post_notify(std::string sid, std::string body) {
  auto* cmd = new SonosCommand{};
  cmd->type = CommandType::Notify;
  cmd->sid = std::move(sid);
  cmd->body = std::move(body);
  return post_command(cmd);
}

}  // namespace coprocessor
