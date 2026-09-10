//! Wi-Fi STA, SNTP clock sync, and Picoserve background HTTP server.

use alloc::string::String;
use core::fmt::Write as FmtWrite;

use embassy_executor::Spawner;
use embassy_net::dns::DnsQueryType;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{Config as NetConfig, Runner, Stack, StackResources};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use enc_state::{AppState, ConnState, parse_ntp_reply};
use esp_hal::peripherals::WIFI;
use esp_hal::rng::Rng;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config as WifiConfig, ControllerConfig, Interface, WifiController};
use picoserve::extract::Json as ExtractJson;
use picoserve::response::{File, Json, StatusCode};
use picoserve::routing::{get, get_service};
use picoserve::{AppBuilder, AppRouter};
use static_cell::StaticCell;

use crate::storage::MacropadSettings;

/// Build-time Wi-Fi credentials.
const SSID: &str = match option_env!("WIFI_SSID") {
    Some(value) => value,
    None => "",
};
const PASSWORD: &str = match option_env!("WIFI_PASSWORD") {
    Some(value) => value,
    None => "",
};

/// Backoff between failed association attempts.
const RETRY: Duration = Duration::from_secs(2);

/// NTP host to query (override at build time; defaults to the NTP pool).
const NTP_HOST: &str = match option_env!("NTP_HOST") {
    Some(value) => value,
    None => "pool.ntp.org",
};
/// Local UDP port for the SNTP client.
const SNTP_LOCAL_PORT: u16 = 50_123;
/// Re-sync interval once the clock is set.
const SNTP_INTERVAL: Duration = Duration::from_secs(3_600);
/// Backoff between failed SNTP attempts.
const SNTP_RETRY: Duration = Duration::from_secs(30);

/// Number of concurrent Picoserve HTTP worker tasks.
const WEB_TASK_POOL_SIZE: usize = 3;

/// Socket pool capacity: DHCP + DNS + SNTP + 3 HTTP sockets + headroom.
const SOCKETS: usize = 10;

static RESOURCES: StaticCell<StackResources<SOCKETS>> = StaticCell::new();
static CONFIG: picoserve::Config = picoserve::Config::const_default();

/// Telemetry response returned by `/api/status`.
#[derive(serde::Serialize)]
struct StatusResponse<'a> {
    version: &'a str,
    uptime_secs: u32,
    ip: heapless::String<16>,
    ble_linked: bool,
    heap_free: usize,
    cpu0_usage: u8,
    cpu1_usage: u8,
}

/// Picoserve Application definition.
struct App;

impl AppBuilder for App {
    type PathRouter = impl picoserve::routing::PathRouter;

    fn build_app(self) -> picoserve::Router<Self::PathRouter> {
        picoserve::Router::new()
            .route(
                "/",
                get_service(File::html(include_str!("../web/index.html"))),
            )
            .route("/favicon.ico", get(async || StatusCode::NO_CONTENT))
            .route(
                "/api/status",
                get(async || {
                    let mut ip_str = heapless::String::new();
                    if let Some(([a, b, c, d], _)) = crate::radio::wifi_info() {
                        let _ = write!(ip_str, "{a}.{b}.{c}.{d}");
                    }
                    let status = StatusResponse {
                        version: env!("CARGO_PKG_VERSION"),
                        uptime_secs: uptime_secs(),
                        ip: ip_str,
                        ble_linked: crate::radio::ble_linked(),
                        heap_free: esp_alloc::HEAP.free(),
                        cpu0_usage: crate::cpu_metrics::cpu0_usage_pct(),
                        cpu1_usage: crate::cpu_metrics::cpu1_usage_pct(),
                    };
                    Json(status)
                }),
            )
            .route(
                "/api/macropad",
                get(async || {
                    let settings = crate::storage::get_macropad_settings().await;
                    Json(settings)
                })
                .post(
                    async |ExtractJson(settings): ExtractJson<MacropadSettings>| {
                        match crate::storage::save_macropad_settings(settings).await {
                            Ok(()) => Json(true),
                            Err(_) => Json(false),
                        }
                    },
                ),
            )
    }
}

/// Brings up Wi-Fi STA, initializes embassy-net, and spawns net/conn/sntp/web tasks.
pub fn start(
    spawner: &Spawner,
    wifi: WIFI<'static>,
    seed: u64,
    state: &'static AppState,
) -> Option<Stack<'static>> {
    if SSID.is_empty() {
        log::warn!("net: WIFI_SSID unset at build time — Wi-Fi disabled");
        return None;
    }

    let (controller, interfaces) = match esp_radio::wifi::new(wifi, ControllerConfig::default()) {
        Ok(pair) => pair,
        Err(e) => {
            log::error!("net: wifi init failed: {e:?}");
            return None;
        }
    };

    let mut dhcp_config = embassy_net::DhcpConfig::default();
    let mut hostname = heapless::String::new();
    let _ = hostname.push_str("t-encoder");
    dhcp_config.hostname = Some(hostname);

    let resources = RESOURCES.init(StackResources::new());
    let (stack, runner) = embassy_net::new(
        interfaces.station,
        NetConfig::dhcpv4(dhcp_config),
        resources,
        seed,
    );

    let Ok(net_token) = net_task(runner) else {
        log::error!("net: failed to spawn net_task");
        return None;
    };
    spawner.spawn(net_token);

    let Ok(conn_token) = connection_task(controller, state) else {
        log::error!("net: failed to spawn connection_task");
        return None;
    };
    spawner.spawn(conn_token);

    let Ok(sntp_token) = sntp_task(stack, state) else {
        log::error!("net: failed to spawn sntp_task");
        return None;
    };
    spawner.spawn(sntp_token);

    let app = picoserve::make_static!(AppRouter<App>, App.build_app());

    for task_id in 0..WEB_TASK_POOL_SIZE {
        match web_task(task_id, stack, app) {
            Ok(token) => spawner.spawn(token),
            Err(_) => log::error!("net: failed to spawn web_task {task_id}"),
        }
    }

    Some(stack)
}

/// Runs the embassy-net smoltcp poll loop.
#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface<'static>>) -> ! {
    runner.run().await
}

/// Drives Wi-Fi association and automatic reconnect.
#[embassy_executor::task]
async fn connection_task(mut controller: WifiController<'static>, state: &'static AppState) -> ! {
    let config = WifiConfig::Station(
        StationConfig::default()
            .with_ssid(SSID)
            .with_password(String::from(PASSWORD)),
    );
    if let Err(e) = controller.set_config(&config) {
        log::error!("net: set_config failed: {e:?}");
    }

    loop {
        state.set_conn(ConnState::Connecting);
        match controller.connect_async().await {
            Ok(_) => {
                log::info!("net: associated to {SSID}");
                let _ = controller.wait_for_disconnect_async().await;
                log::warn!("net: link lost");
                state.set_conn(ConnState::Disconnected);
                state.clear_ip();
            }
            Err(e) => {
                log::warn!("net: connect failed: {e:?}");
                state.set_conn(ConnState::Disconnected);
                Timer::after(RETRY).await;
            }
        }
    }
}

/// Periodically syncs system wall-clock time from SNTP.
#[embassy_executor::task]
async fn sntp_task(stack: Stack<'static>, state: &'static AppState) -> ! {
    loop {
        stack.wait_config_up().await;
        if let Some(epoch) = sntp_sync(stack).await {
            let uptime = u32::try_from(Instant::now().as_secs()).unwrap_or(0);
            state.set_time_sync(epoch, uptime);
            log::info!("sntp: synced unix={epoch}");
            Timer::after(SNTP_INTERVAL).await;
        } else {
            log::warn!("sntp: sync failed");
            Timer::after(SNTP_RETRY).await;
        }
    }
}

/// Performs a single SNTP request-response exchange.
async fn sntp_sync(stack: Stack<'static>) -> Option<u32> {
    let addresses = stack.dns_query(NTP_HOST, DnsQueryType::A).await.ok()?;
    let server = addresses.first().copied()?;

    let rng = Rng::new();
    let [n0, n1, n2, n3] = rng.random().to_be_bytes();
    let [n4, n5, n6, n7] = rng.random().to_be_bytes();
    let nonce = [n0, n1, n2, n3, n4, n5, n6, n7];

    let mut rx_meta = [PacketMetadata::EMPTY; 2];
    let mut rx_buf = [0u8; 256];
    let mut tx_meta = [PacketMetadata::EMPTY; 2];
    let mut tx_buf = [0u8; 256];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    socket.bind(SNTP_LOCAL_PORT).ok()?;

    let mut request = [0u8; 48];
    *request.first_mut()? = 0x1B;
    request.get_mut(40..48)?.copy_from_slice(&nonce);
    socket.send_to(&request, (server, 123)).await.ok()?;

    with_timeout(
        Duration::from_secs(5),
        socket.recv_from_with(|data, meta| {
            if meta.endpoint.addr == server && meta.endpoint.port == 123 {
                parse_ntp_reply(data, &nonce)
            } else {
                None
            }
        }),
    )
    .await
    .ok()
    .flatten()
}

/// Background worker task running Picoserve HTTP listener.
#[embassy_executor::task(pool_size = WEB_TASK_POOL_SIZE)]
async fn web_task(task_id: usize, stack: Stack<'static>, app: &'static AppRouter<App>) -> ! {
    let port = 80;
    let mut receive_buffer = [0u8; 1024];
    let mut transmit_buffer = [0u8; 1024];
    let mut http_buffer = [0u8; 2048];

    picoserve::Server::new(app, &CONFIG, &mut http_buffer)
        .listen_and_serve(
            task_id,
            stack,
            port,
            &mut receive_buffer,
            &mut transmit_buffer,
        )
        .await
        .into_never()
}

/// Device uptime in seconds.
fn uptime_secs() -> u32 {
    u32::try_from(Instant::now().as_secs()).unwrap_or(0)
}
