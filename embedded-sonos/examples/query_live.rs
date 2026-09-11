#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stdout,
    clippy::cast_possible_truncation
)]

use core::net::Ipv4Addr;
use embedded_io_async::{ErrorType, Read, Write};
use embedded_sonos::{
    SONOS_DEFAULT_PORT, VolumeChannel, get_position_info, get_transport_info, get_volume,
    get_zone_group_state,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Simple adapter converting Tokio's AsyncRead/AsyncWrite to embedded-io-async 0.7 traits.
struct TokioStream(TcpStream);

impl ErrorType for TokioStream {
    type Error = embedded_io_async::ErrorKind;
}

impl Read for TokioStream {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.0
            .read(buf)
            .await
            .map_err(|_| embedded_io_async::ErrorKind::Other)
    }
}

impl Write for TokioStream {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.0
            .write(buf)
            .await
            .map_err(|_| embedded_io_async::ErrorKind::Other)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.0
            .flush()
            .await
            .map_err(|_| embedded_io_async::ErrorKind::Other)
    }
}

struct TokioStreamVec<'a>(&'a mut Vec<u8>);

impl ErrorType for TokioStreamVec<'_> {
    type Error = embedded_io_async::ErrorKind;
}

impl Write for TokioStreamVec<'_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let speaker_ip = Ipv4Addr::new(192, 168, 9, 105);

    println!("Connecting to Sonos at {speaker_ip}:{SONOS_DEFAULT_PORT}...");

    // 1. Query Transport Info
    {
        let socket = TcpStream::connect((speaker_ip, SONOS_DEFAULT_PORT)).await?;
        let mut stream = TokioStream(socket);
        let state = get_transport_info(&mut stream, speaker_ip, SONOS_DEFAULT_PORT).await?;
        println!("Playback state: {state:?}");
    }

    // 2. Query Volume
    {
        let socket = TcpStream::connect((speaker_ip, SONOS_DEFAULT_PORT)).await?;
        let mut stream = TokioStream(socket);
        let vol = get_volume(
            &mut stream,
            speaker_ip,
            SONOS_DEFAULT_PORT,
            VolumeChannel::Master,
        )
        .await?;
        println!("Master volume: {vol}");
    }

    // 3. Query Position & Track Metadata
    {
        let socket = TcpStream::connect((speaker_ip, SONOS_DEFAULT_PORT)).await?;
        let mut stream = TokioStream(socket);
        let track_opt = get_position_info(&mut stream, speaker_ip, SONOS_DEFAULT_PORT).await?;
        if let Some(track) = track_opt {
            println!(
                "Track #{}: \"{}\" by \"{}\" (Album: \"{}\")",
                track.track_number,
                track.title,
                track.artist.as_deref().unwrap_or("Unknown"),
                track.album.as_deref().unwrap_or("Unknown")
            );
            println!(
                "Elapsed: {}s / {}s",
                track.elapsed_seconds, track.duration_seconds
            );
            if let Some(art) = &track.album_art_uri {
                println!("Album Art URI: {art}");
                let socket = TcpStream::connect((speaker_ip, SONOS_DEFAULT_PORT)).await?;
                let mut stream = TokioStream(socket);
                let mut img_buf = Vec::new();
                let mut img_stream = TokioStreamVec(&mut img_buf);
                let bytes = embedded_sonos::stream_album_art(
                    &mut stream,
                    speaker_ip,
                    SONOS_DEFAULT_PORT,
                    art,
                    &mut img_stream,
                )
                .await?;
                println!(
                    "Successfully streamed album art: {bytes} bytes (starts with JPEG SOI: {:02X?})",
                    &img_buf[..2]
                );
            }
        } else {
            println!("No track currently loaded or playing.");
        }
    }

    // 4. Query Household Topology
    {
        let socket = TcpStream::connect((speaker_ip, SONOS_DEFAULT_PORT)).await?;
        let mut stream = TokioStream(socket);
        let topology = get_zone_group_state(&mut stream, speaker_ip, SONOS_DEFAULT_PORT).await?;
        println!("\nDiscovered {} zone groups:", topology.groups.len());
        for group in &topology.groups {
            println!(
                "- Group \"{}\" (Coordinator: {:?}):",
                group.name, group.coordinator_ip
            );
            for member in &group.members {
                println!(
                    "    • {} [{}] at {} (coordinator: {})",
                    member.name, member.uuid, member.ip, member.is_coordinator
                );
            }
        }
    }

    Ok(())
}
