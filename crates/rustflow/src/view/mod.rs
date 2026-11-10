//! See the Linux screen in a window on the Mac.
//!
//! Linux records its screen through the ScreenCast portal, encodes it as
//! H.264 with GStreamer and streams it over its own TCP connection (main
//! port + 2), only while the Mac's window is open. The Mac decodes it in
//! hardware and shows it in a window; clicks and keys in that window go to
//! Linux over the main connection, like when the pointer crosses over.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use bytes::{BufMut, Bytes, BytesMut};

use crate::net;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "linux", test))]
mod mkv;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub mod window;

#[cfg(target_os = "linux")]
pub use linux::run;
#[cfg(target_os = "macos")]
pub use macos::run;

const RETRY_AFTER: Duration = Duration::from_secs(2);

/// The screen port is the main port + 2.
pub fn view_addr(addr: &str) -> Result<String> {
    net::side_addr(addr, 2)
}

/// One frame on the screen connection (after the usual Hello/Welcome).
#[derive(Debug, Clone, PartialEq)]
pub enum Packet {
    /// Something to show while there is no picture ("waiting for permission").
    Status(String),
    /// H.264 decoder setup (an `avcC` record); comes before the first frame.
    Format(Bytes),
    /// One encoded picture: NAL units with 4-byte big-endian lengths.
    Frame { key: bool, data: Bytes },
}

const STATUS: u8 = 0;
const FORMAT: u8 = 1;
const FRAME: u8 = 2;
const KEY_FRAME: u8 = 3;

impl Packet {
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub fn encode(&self) -> Bytes {
        let (tag, body): (u8, &[u8]) = match self {
            Packet::Status(text) => (STATUS, text.as_bytes()),
            Packet::Format(avcc) => (FORMAT, avcc),
            Packet::Frame { key, data } => (if *key { KEY_FRAME } else { FRAME }, data),
        };
        let mut out = BytesMut::with_capacity(1 + body.len());
        out.put_u8(tag);
        out.put_slice(body);
        out.freeze()
    }

    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub fn decode(mut bytes: Bytes) -> Result<Self> {
        if bytes.is_empty() {
            bail!("empty screen packet");
        }
        let tag = bytes.split_to(1)[0];
        Ok(match tag {
            STATUS => Packet::Status(String::from_utf8_lossy(&bytes).into_owned()),
            FORMAT => Packet::Format(bytes),
            FRAME => Packet::Frame { key: false, data: bytes },
            KEY_FRAME => Packet::Frame { key: true, data: bytes },
            other => bail!("unknown screen packet {other}"),
        })
    }
}

/// H.264 parameter sets from an `avcC` record.
#[derive(Debug, PartialEq)]
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub struct ParameterSets {
    /// SPS then PPS NAL units.
    pub sets: Vec<Vec<u8>>,
    /// Size of each NAL length prefix in frames (normally 4).
    pub length_size: usize,
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
pub fn parse_avcc(avcc: &[u8]) -> Result<ParameterSets> {
    let mut r = avcc;
    let mut take = |n: usize| -> Result<&[u8]> {
        if r.len() < n {
            bail!("avcC record is truncated");
        }
        let (head, rest) = r.split_at(n);
        r = rest;
        Ok(head)
    };
    let head = take(6)?;
    if head[0] != 1 {
        bail!("unknown avcC version {}", head[0]);
    }
    let length_size = (head[4] & 3) as usize + 1;
    let mut sets = Vec::new();
    let sps_count = head[5] & 0x1f;
    for _ in 0..sps_count {
        let len = u16::from_be_bytes(take(2)?.try_into()?) as usize;
        sets.push(take(len)?.to_vec());
    }
    let pps_count = take(1)?[0];
    for _ in 0..pps_count {
        let len = u16::from_be_bytes(take(2)?.try_into()?) as usize;
        sets.push(take(len)?.to_vec());
    }
    if sps_count == 0 || pps_count == 0 {
        bail!("avcC record has no SPS or PPS");
    }
    Ok(ParameterSets { sets, length_size })
}

/// Mac: whether the screen window is open, and where its stream goes.
/// Linux: nothing to hold.
#[derive(Clone)]
pub struct View(#[cfg_attr(target_os = "linux", allow(dead_code))] Arc<Inner>);

#[cfg_attr(target_os = "linux", allow(dead_code))]
struct Inner {
    /// True while the window is open.
    wanted: tokio::sync::watch::Sender<bool>,
    /// Bumped to drop the stream and start a fresh one (decoder trouble).
    restart: tokio::sync::Notify,
    sink: Box<dyn Fn(Event) + Send + Sync>,
}

/// What the Mac window gets from the stream.
#[derive(Debug)]
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub enum Event {
    Status(String),
    Format(ParameterSets),
    Frame { key: bool, data: Bytes },
}

impl View {
    pub fn new(sink: impl Fn(Event) + Send + Sync + 'static) -> Self {
        Self(Arc::new(Inner {
            wanted: tokio::sync::watch::Sender::new(false),
            restart: tokio::sync::Notify::new(),
            sink: Box::new(sink),
        }))
    }

    /// Open (true) or close (false) the stream.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub fn set_wanted(&self, on: bool) {
        self.0.wanted.send_replace(on);
    }

    /// Reconnect to get a fresh stream (it starts with a key frame).
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub fn restart(&self) {
        self.0.restart.notify_one();
    }

    #[cfg_attr(target_os = "linux", allow(dead_code))]
    fn emit(&self, event: Event) {
        (self.0.sink)(event)
    }
}

/// `avcC` record for tests: one SPS and one PPS, 4-byte lengths.
#[cfg(test)]
fn test_avcc(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut v = vec![1, sps[1], sps[2], sps[3], 0xff, 0xe1];
    v.extend((sps.len() as u16).to_be_bytes());
    v.extend(sps);
    v.push(1);
    v.extend((pps.len() as u16).to_be_bytes());
    v.extend(pps);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_round_trip() {
        for p in [
            Packet::Status("waiting".into()),
            Packet::Format(Bytes::from_static(&[1, 2, 3])),
            Packet::Frame { key: true, data: Bytes::from_static(&[0, 0, 0, 1, 0x65]) },
            Packet::Frame { key: false, data: Bytes::new() },
        ] {
            assert_eq!(Packet::decode(p.encode()).unwrap(), p);
        }
        assert!(Packet::decode(Bytes::new()).is_err());
    }

    #[test]
    fn view_port_is_main_port_plus_two() {
        assert_eq!(view_addr("192.168.1.2:24801").unwrap(), "192.168.1.2:24803");
    }

    #[test]
    fn parses_avcc() {
        let sps = [0x67, 0x42, 0xc0, 0x28, 0xda];
        let pps = [0x68, 0xce, 0x3c, 0x80];
        let ps = parse_avcc(&test_avcc(&sps, &pps)).unwrap();
        assert_eq!(ps, ParameterSets { sets: vec![sps.to_vec(), pps.to_vec()], length_size: 4 });
        assert!(parse_avcc(&[1, 2, 3]).is_err());
    }
}
