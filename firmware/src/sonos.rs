//! Background Sonos network worker and UI bridge.
//!
//! Connects to Sonos speakers over TCP (SOAP), updates the shared [`SonosSnapshot`],
//! and executes playback commands dispatched by [`apps::Sonos`].

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::net::Ipv4Addr;

use apps::{GroupSummary, NowPlayingData, SonosCommand, SonosSnapshot};
use embassy_net::tcp::TcpSocket;
use embassy_net::{IpAddress, IpEndpoint, Stack};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer, with_timeout};
use embedded_sonos::{
    SONOS_DEFAULT_PORT, TransportState, VolumeChannel, get_position_info, get_transport_info,
    get_volume, get_zone_group_state, next, pause, play, previous, set_relative_volume,
};

/// Build-time seed host IP (can be overridden via SONOS_HOST env var).
const SEED_HOST: &str = match option_env!("SONOS_HOST") {
    Some(val) => val,
    None => "192.168.9.105",
};

/// Shared snapshot published to the UI thread.
static SNAPSHOT: BlockingMutex<CriticalSectionRawMutex, RefCell<SonosSnapshot>> =
    BlockingMutex::new(RefCell::new(SonosSnapshot {
        groups: Vec::new(),
        active_now_playing: NowPlayingData {
            track_title: heapless::String::new(),
            track_artist: heapless::String::new(),
            track_album: heapless::String::new(),
            elapsed_seconds: 0,
            duration_seconds: 0,
            volume: 20,
            is_playing: false,
        },
        revision: 0,
    }));

/// Command channel from UI to the network worker.
static COMMAND_CHANNEL: Channel<CriticalSectionRawMutex, SonosCommand, 8> = Channel::new();

/// Cached list of group coordinator IPs in index order.
static COORDINATOR_IPS: BlockingMutex<CriticalSectionRawMutex, RefCell<Vec<Option<Ipv4Addr>>>> =
    BlockingMutex::new(RefCell::new(Vec::new()));

/// Active selected group index.
static ACTIVE_GROUP_IDX: BlockingMutex<CriticalSectionRawMutex, RefCell<usize>> =
    BlockingMutex::new(RefCell::new(0));

/// Synchronous snapshot getter provided to [`apps::SonosFactory`].
#[must_use]
pub fn get_sonos_snapshot_sync() -> SonosSnapshot {
    SNAPSHOT.lock(|cell| cell.borrow().clone())
}

/// Synchronous command sink provided to [`apps::SonosFactory`].
pub fn send_sonos_command_sync(cmd: SonosCommand) {
    if let SonosCommand::SelectGroup { group_idx } = cmd {
        ACTIVE_GROUP_IDX.lock(|cell| {
            *cell.borrow_mut() = group_idx;
        });
    }
    let _ = COMMAND_CHANNEL.try_send(cmd);
}

/// Background worker task that talks to Sonos over `embassy-net`.
#[embassy_executor::task]
pub async fn sonos_worker_task(stack: Stack<'static>) {
    let seed_ip = match SEED_HOST.parse::<Ipv4Addr>() {
        Ok(ip) => ip,
        Err(_) => {
            log::error!("sonos: invalid SEED_HOST {:?}", SEED_HOST);
            return;
        }
    };

    log::info!("sonos: worker started with seed IP {}", seed_ip);

    let mut rx_buf = [0u8; 4096];
    let mut tx_buf = [0u8; 2048];

    loop {
        // Wait for Wi-Fi association and DHCP IP assignment
        if !stack.is_link_up() || stack.config_v4().is_none() {
            Timer::after(Duration::from_secs(2)).await;
            continue;
        }

        // 1. Discover or refresh household topology from seed speaker
        let coordinator = get_coordinator_ip(0);
        let target_ip = coordinator.unwrap_or(seed_ip);

        if let Err(e) = refresh_topology(stack, target_ip, &mut rx_buf, &mut tx_buf).await {
            log::warn!("sonos: topology refresh failed: {:?}", e);
        }

        // 2. Command processing loop with timeout for periodic poll
        let active_idx = ACTIVE_GROUP_IDX.lock(|c| *c.borrow());
        if let Some(coord_ip) = get_coordinator_ip(active_idx) {
            // Refresh playback status of active group
            if let Err(e) =
                refresh_now_playing(stack, coord_ip, active_idx, &mut rx_buf, &mut tx_buf).await
            {
                log::warn!("sonos: now playing refresh failed: {:?}", e);
            }
        }

        // Wait up to 1.5s for incoming user commands or poll tick
        let wait_result =
            with_timeout(Duration::from_millis(1500), COMMAND_CHANNEL.receive()).await;

        if let Ok(cmd) = wait_result {
            handle_command(stack, cmd, &mut rx_buf, &mut tx_buf).await;
        }
    }
}

fn get_coordinator_ip(group_idx: usize) -> Option<Ipv4Addr> {
    COORDINATOR_IPS.lock(|c| c.borrow().get(group_idx).copied().flatten())
}

async fn refresh_topology(
    stack: Stack<'static>,
    ip: Ipv4Addr,
    rx_buf: &mut [u8],
    tx_buf: &mut [u8],
) -> Result<(), ()> {
    let mut socket = TcpSocket::new(stack, rx_buf, tx_buf);
    socket.set_timeout(Some(Duration::from_secs(4)));
    let endpoint = IpEndpoint::new(IpAddress::Ipv4(ip), SONOS_DEFAULT_PORT);

    if socket.connect(endpoint).await.is_err() {
        return Err(());
    }

    let topology = match get_zone_group_state(&mut socket, ip, SONOS_DEFAULT_PORT).await {
        Ok(top) => top,
        Err(_) => return Err(()),
    };

    let mut coords = Vec::new();
    for group in &topology.groups {
        coords.push(group.coordinator_ip);
    }

    COORDINATOR_IPS.lock(|c| {
        *c.borrow_mut() = coords;
    });

    SNAPSHOT.lock(|c| {
        let mut snap = c.borrow_mut();
        let mut summaries = Vec::new();

        for group in &topology.groups {
            let mut members_str = String::new();
            for (i, m) in group.members.iter().enumerate() {
                if i > 0 {
                    members_str.push_str(", ");
                }
                members_str.push_str(&m.name);
            }

            let prev_group = snap
                .groups
                .iter()
                .find(|g| g.name.as_str() == group.name.as_str());
            let summary = prev_group.map_or("", |g| g.playing_summary.as_str());
            let is_playing = prev_group.map_or(false, |g| g.is_playing);

            summaries.push(GroupSummary::new(
                &group.name,
                &members_str,
                summary,
                is_playing,
            ));
        }

        snap.groups = summaries;
        snap.revision = snap.revision.wrapping_add(1);
    });

    Ok(())
}

async fn refresh_now_playing(
    stack: Stack<'static>,
    coord_ip: Ipv4Addr,
    group_idx: usize,
    rx_buf: &mut [u8],
    tx_buf: &mut [u8],
) -> Result<(), ()> {
    let endpoint = IpEndpoint::new(IpAddress::Ipv4(coord_ip), SONOS_DEFAULT_PORT);

    let transport_state = {
        let mut socket = TcpSocket::new(stack, rx_buf, tx_buf);
        socket.set_timeout(Some(Duration::from_secs(3)));
        if socket.connect(endpoint).await.is_err() {
            return Err(());
        }
        let res = get_transport_info(&mut socket, coord_ip, SONOS_DEFAULT_PORT)
            .await
            .unwrap_or(TransportState::Stopped);
        socket.close();
        res
    };

    // Volume query
    let volume: u8 = {
        let mut socket = TcpSocket::new(stack, rx_buf, tx_buf);
        socket.set_timeout(Some(Duration::from_secs(3)));
        let mut vol_res = 20;
        if socket.connect(endpoint).await.is_ok() {
            if let Ok(vol) = get_volume(
                &mut socket,
                coord_ip,
                SONOS_DEFAULT_PORT,
                VolumeChannel::Master,
            )
            .await
            {
                vol_res = u8::try_from(vol).unwrap_or(0);
            }
            socket.close();
        }
        vol_res
    };

    // Position & metadata query
    let track_opt = {
        let mut socket = TcpSocket::new(stack, rx_buf, tx_buf);
        socket.set_timeout(Some(Duration::from_secs(3)));
        if socket.connect(endpoint).await.is_err() {
            return Err(());
        }
        let res = get_position_info(&mut socket, coord_ip, SONOS_DEFAULT_PORT)
            .await
            .map_err(|_| ());
        socket.close();
        res?
    };

    let is_playing = transport_state == TransportState::Playing;

    let mut title = heapless::String::new();
    let mut artist = heapless::String::new();
    let mut album = heapless::String::new();
    let mut elapsed_seconds = 0;
    let mut duration_seconds = 0;

    if let Some(track) = track_opt {
        let _ = title.push_str(&track.title);
        if let Some(a) = &track.artist {
            let _ = artist.push_str(a);
        }
        if let Some(al) = &track.album {
            let _ = album.push_str(al);
        }
        elapsed_seconds = track.elapsed_seconds;
        duration_seconds = track.duration_seconds;
    }

    let mut summary = heapless::String::new();
    if !title.is_empty() {
        let _ = summary.push_str(&title);
        if !artist.is_empty() {
            let _ = summary.push_str(" - ");
            let _ = summary.push_str(&artist);
        }
    } else if is_playing {
        let _ = summary.push_str("Playing");
    } else {
        let _ = summary.push_str("Paused");
    }

    SNAPSHOT.lock(|c| {
        let mut snap = c.borrow_mut();
        snap.active_now_playing = NowPlayingData {
            track_title: title,
            track_artist: artist,
            track_album: album,
            elapsed_seconds,
            duration_seconds,
            volume,
            is_playing,
        };
        if let Some(g) = snap.groups.get_mut(group_idx) {
            g.playing_summary = summary;
            g.is_playing = is_playing;
        }
        snap.revision = snap.revision.wrapping_add(1);
    });

    Ok(())
}

async fn handle_command(
    stack: Stack<'static>,
    cmd: SonosCommand,
    rx_buf: &mut [u8],
    tx_buf: &mut [u8],
) {
    let group_idx = match cmd {
        SonosCommand::SelectGroup { group_idx }
        | SonosCommand::TogglePlayPause { group_idx }
        | SonosCommand::NextTrack { group_idx }
        | SonosCommand::PreviousTrack { group_idx }
        | SonosCommand::AdjustVolume { group_idx, .. } => group_idx,
    };

    let Some(coord_ip) = get_coordinator_ip(group_idx) else {
        return;
    };

    let endpoint = IpEndpoint::new(IpAddress::Ipv4(coord_ip), SONOS_DEFAULT_PORT);

    match cmd {
        SonosCommand::SelectGroup { .. } => {}
        SonosCommand::TogglePlayPause { .. } => {
            let state = {
                let mut socket = TcpSocket::new(stack, rx_buf, tx_buf);
                socket.set_timeout(Some(Duration::from_secs(3)));
                if socket.connect(endpoint).await.is_err() {
                    return;
                }
                let s = get_transport_info(&mut socket, coord_ip, SONOS_DEFAULT_PORT)
                    .await
                    .unwrap_or(TransportState::Stopped);
                socket.close();
                s
            };

            let mut socket = TcpSocket::new(stack, rx_buf, tx_buf);
            socket.set_timeout(Some(Duration::from_secs(3)));
            if socket.connect(endpoint).await.is_ok() {
                if state == TransportState::Playing {
                    let _ = pause(&mut socket, coord_ip, SONOS_DEFAULT_PORT).await;
                } else {
                    let _ = play(&mut socket, coord_ip, SONOS_DEFAULT_PORT).await;
                }
                socket.close();
            }
        }
        other => {
            let mut socket = TcpSocket::new(stack, rx_buf, tx_buf);
            socket.set_timeout(Some(Duration::from_secs(3)));
            if socket.connect(endpoint).await.is_err() {
                return;
            }
            match other {
                SonosCommand::NextTrack { .. } => {
                    let _ = next(&mut socket, coord_ip, SONOS_DEFAULT_PORT).await;
                }
                SonosCommand::PreviousTrack { .. } => {
                    let _ = previous(&mut socket, coord_ip, SONOS_DEFAULT_PORT).await;
                }
                SonosCommand::AdjustVolume { delta, .. } => {
                    let adj = i16::try_from(delta).unwrap_or(0);
                    let _ = set_relative_volume(
                        &mut socket,
                        coord_ip,
                        SONOS_DEFAULT_PORT,
                        VolumeChannel::Master,
                        adj,
                    )
                    .await;
                }
                _ => {}
            }
            socket.close();
        }
    }
}
