//! Wake-on-LAN magic packet.

use std::net::UdpSocket;

use anyhow::{Context, Result, bail};

fn parse_mac(mac: &str) -> Result<[u8; 6]> {
    let parts: Vec<&str> = mac.split([':', '-']).collect();
    if parts.len() != 6 {
        bail!("bad MAC address {mac:?}");
    }
    let mut out = [0u8; 6];
    for (byte, part) in out.iter_mut().zip(parts) {
        *byte = u8::from_str_radix(part, 16).with_context(|| format!("bad MAC address {mac:?}"))?;
    }
    Ok(out)
}

/// 6 × 0xFF followed by the MAC address 16 times.
fn magic_packet(mac: [u8; 6]) -> [u8; 102] {
    let mut pkt = [0xFFu8; 102];
    for chunk in pkt[6..].chunks_exact_mut(6) {
        chunk.copy_from_slice(&mac);
    }
    pkt
}

pub fn send(mac: &str, broadcast: &str) -> Result<()> {
    let pkt = magic_packet(parse_mac(mac)?);
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_broadcast(true)?;
    sock.send_to(&pkt, (broadcast, 9))
        .with_context(|| format!("cannot send magic packet to {broadcast}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_layout() {
        let mac = parse_mac("02:00:5e:10:20:30").unwrap();
        let pkt = magic_packet(mac);
        assert_eq!(&pkt[..6], &[0xFF; 6]);
        assert_eq!(&pkt[6..12], &mac);
        assert_eq!(&pkt[96..], &mac);
    }

    #[test]
    fn rejects_bad_mac() {
        assert!(parse_mac("02:00:5e").is_err());
        assert!(parse_mac("zz:a1:59:2d:35:9d").is_err());
    }
}
