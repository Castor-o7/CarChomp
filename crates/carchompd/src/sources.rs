//! Source tasks. Each one connects to something, turns what it hears into
//! `Observation`s on the bus, and reconnects forever if the connection drops.
//! A new kind of source is one more function here.

use crate::{Bus, Event};
use carchomp_core::{Observation, aprs, gpsd, kiss};
use std::{io, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};

const RETRY: Duration = Duration::from_secs(5);
/// A peer that vanished without closing the connection (power cut, Wi-Fi
/// drop) is only noticed by its silence. gpsd reports every second while it
/// has a receiver; a TNC can be quiet for minutes on an empty channel.
const GPSD_SILENCE: Duration = Duration::from_secs(60);
const KISS_SILENCE: Duration = Duration::from_secs(15 * 60);

/// Read with a deadline; silence past it ends the session like an error.
async fn within<T>(limit: Duration, read: impl Future<Output = io::Result<T>>) -> io::Result<T> {
    tokio::time::timeout(limit, read)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, format!("nothing heard for {} s", limit.as_secs())))?
}

pub async fn gpsd(addr: String, source: i16, bus: Bus) {
    loop {
        retry("gpsd", &addr, gpsd_session(&addr, source, &bus).await).await;
    }
}

pub async fn aprs_kiss(addr: String, source: i16, bus: Bus) {
    loop {
        retry("aprs", &addr, aprs_kiss_session(&addr, source, &bus).await).await;
    }
}

async fn gpsd_session(addr: &str, source: i16, bus: &Bus) -> io::Result<()> {
    let mut stream = BufReader::new(TcpStream::connect(addr).await?);
    stream.get_mut().write_all(gpsd::WATCH.as_bytes()).await?;
    let mut lines = stream.lines();
    // Per connection: a new receiver may not report speed at all.
    let mut partials = gpsd::Partials::default();
    while let Some(line) = within(GPSD_SILENCE, lines.next_line()).await? {
        if let Some(fix) = gpsd::parse_fix(&line).filter(|fix| !partials.is_partial(fix)) {
            publish(bus, source, Observation::Fix(fix));
        }
    }
    Ok(())
}

async fn aprs_kiss_session(addr: &str, source: i16, bus: &Bus) -> io::Result<()> {
    let mut stream = TcpStream::connect(addr).await?;
    let mut decoder = kiss::Decoder::default();
    let mut buf = [0u8; 1024];
    loop {
        let n = within(KISS_SILENCE, stream.read(&mut buf)).await?;
        if n == 0 {
            return Ok(());
        }
        decoder.push(&buf[..n], |frame| {
            if let Some(packet) = aprs::parse_frame(frame) {
                publish(bus, source, Observation::Aprs(Box::new(packet)));
            }
        });
    }
}

fn publish(bus: &Bus, source: i16, obs: Observation) {
    // An error only means nobody is listening right now.
    let _ = bus.send(Event { source, obs });
}

/// Log how a session ended, then wait before the caller tries again.
async fn retry(name: &str, addr: &str, ended: io::Result<()>) {
    match ended {
        Ok(()) => tracing::warn!("{name}: {addr} closed the connection"),
        Err(e) => tracing::warn!("{name}: {addr}: {e}"),
    }
    tokio::time::sleep(RETRY).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn silence_is_an_error() {
        let err = within(Duration::from_millis(10), std::future::pending::<io::Result<()>>()).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert_eq!(within(Duration::from_secs(1), async { Ok(7) }).await.unwrap(), 7);
    }
}
