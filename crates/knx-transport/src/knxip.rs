//! KNXnet/IP frames (`03_08_02 Core`, `03_08_03 Management`,
//! `03_08_04 Tunnelling`), for the tunnelling client ([`crate::ip`]) and
//! knx-sim's server.
//!
//! Every frame starts with the 6-octet header (`03_08_02 §7.1`):
//!
//! ```text
//! 06h  10h  service type (2)  total length (2)
//! ```

use anyhow::{bail, Result};
use std::net::{Ipv4Addr, SocketAddrV4};

pub const HEADER_SIZE_10: u8 = 0x06;
pub const KNXNETIP_VERSION_10: u8 = 0x10;
pub const PORT: u16 = 3671;
/// KNXnet/IP system setup multicast address (`03_08_02 §8.5.2.1`).
pub const SYSTEM_SETUP_MULTICAST: Ipv4Addr = Ipv4Addr::new(224, 0, 23, 12);

/// Service type identifiers (`03_08_02 §7.4.1`, `03_08_03 §4.2.1`,
/// `03_08_04 Table 9`).
pub mod service {
    pub const SEARCH_REQUEST: u16 = 0x0201;
    pub const SEARCH_RESPONSE: u16 = 0x0202;
    /// Core v2, `03_08_02 §7.6.3`; not answered, ETS also sends the v1 search.
    pub const SEARCH_REQUEST_EXTENDED: u16 = 0x020B;
    pub const DESCRIPTION_REQUEST: u16 = 0x0203;
    pub const DESCRIPTION_RESPONSE: u16 = 0x0204;
    pub const CONNECT_REQUEST: u16 = 0x0205;
    pub const CONNECT_RESPONSE: u16 = 0x0206;
    pub const CONNECTIONSTATE_REQUEST: u16 = 0x0207;
    pub const CONNECTIONSTATE_RESPONSE: u16 = 0x0208;
    pub const DISCONNECT_REQUEST: u16 = 0x0209;
    pub const DISCONNECT_RESPONSE: u16 = 0x020A;
    pub const DEVICE_CONFIGURATION_REQUEST: u16 = 0x0310;
    pub const DEVICE_CONFIGURATION_ACK: u16 = 0x0311;
    pub const TUNNELLING_REQUEST: u16 = 0x0420;
    pub const TUNNELLING_ACK: u16 = 0x0421;

    pub fn name(s: u16) -> &'static str {
        match s {
            SEARCH_REQUEST => "SEARCH_REQUEST",
            SEARCH_RESPONSE => "SEARCH_RESPONSE",
            DESCRIPTION_REQUEST => "DESCRIPTION_REQUEST",
            DESCRIPTION_RESPONSE => "DESCRIPTION_RESPONSE",
            CONNECT_REQUEST => "CONNECT_REQUEST",
            CONNECT_RESPONSE => "CONNECT_RESPONSE",
            CONNECTIONSTATE_REQUEST => "CONNECTIONSTATE_REQUEST",
            CONNECTIONSTATE_RESPONSE => "CONNECTIONSTATE_RESPONSE",
            DISCONNECT_REQUEST => "DISCONNECT_REQUEST",
            DISCONNECT_RESPONSE => "DISCONNECT_RESPONSE",
            DEVICE_CONFIGURATION_REQUEST => "DEVICE_CONFIGURATION_REQUEST",
            DEVICE_CONFIGURATION_ACK => "DEVICE_CONFIGURATION_ACK",
            TUNNELLING_REQUEST => "TUNNELLING_REQUEST",
            TUNNELLING_ACK => "TUNNELLING_ACK",
            _ => "?",
        }
    }
}

/// Connection types (`03_08_01 Table 8`).
pub const DEVICE_MGMT_CONNECTION: u8 = 0x03;
pub const TUNNEL_CONNECTION: u8 = 0x04;
/// Tunnelling KNX layer (`03_08_04 Table 10`).
pub const TUNNEL_LINKLAYER: u8 = 0x02;

/// Status codes (`03_08_01 §5.5`).
pub mod status {
    pub const E_NO_ERROR: u8 = 0x00;
    pub const E_HOST_PROTOCOL_TYPE: u8 = 0x01;
    pub const E_CONNECTION_ID: u8 = 0x21;
    pub const E_CONNECTION_TYPE: u8 = 0x22;
    pub const E_NO_MORE_CONNECTIONS: u8 = 0x24;
    pub const E_TUNNELLING_LAYER: u8 = 0x29;
}

/// Split a received datagram into service type and body.
pub fn parse(frame: &[u8]) -> Result<(u16, &[u8])> {
    if frame.len() < 6 {
        bail!("KNXnet/IP frame too short: {} octets", frame.len());
    }
    if frame[0] != HEADER_SIZE_10 || frame[1] != KNXNETIP_VERSION_10 {
        bail!("unsupported KNXnet/IP header {:02X} {:02X}", frame[0], frame[1]);
    }
    let service = u16::from_be_bytes([frame[2], frame[3]]);
    let len = u16::from_be_bytes([frame[4], frame[5]]) as usize;
    if len != frame.len() {
        bail!("KNXnet/IP total length {len}, datagram {} octets", frame.len());
    }
    Ok((service, &frame[6..]))
}

pub fn frame(service: u16, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(6 + body.len());
    out.extend_from_slice(&[HEADER_SIZE_10, KNXNETIP_VERSION_10]);
    out.extend_from_slice(&service.to_be_bytes());
    out.extend_from_slice(&((6 + body.len()) as u16).to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// Host Protocol Address Information (`03_08_02 §8.6.2`), IPv4 UDP only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hpai(pub SocketAddrV4);

pub const IPV4_UDP: u8 = 0x01;

impl Hpai {
    /// Parse the HPAI at the start of `b`; returns it and the rest.
    pub fn parse(b: &[u8]) -> Result<(Hpai, &[u8])> {
        if b.len() < 8 || b[0] != 8 {
            bail!("bad HPAI: {:02X?}", &b[..b.len().min(8)]);
        }
        if b[1] != IPV4_UDP {
            bail!("HPAI host protocol {:02X}h, only UDP is supported", b[1]);
        }
        let ip = Ipv4Addr::new(b[2], b[3], b[4], b[5]);
        let port = u16::from_be_bytes([b[6], b[7]]);
        if ip.is_unspecified() != (port == 0) {
            bail!("invalid HPAI {ip}:{port} (route back needs both zero)");
        }
        Ok((Hpai(SocketAddrV4::new(ip, port)), &b[8..]))
    }

    pub fn encode(&self) -> [u8; 8] {
        let ip = self.0.ip().octets();
        let port = self.0.port().to_be_bytes();
        [8, IPV4_UDP, ip[0], ip[1], ip[2], ip[3], port[0], port[1]]
    }

    /// "Route Back" HPAI: answer to the source of the datagram
    /// (`03_08_02 §8.6.2.2`).
    pub fn is_route_back(&self) -> bool {
        self.0.ip().is_unspecified()
    }

    /// The address to send to: the HPAI, or `source` for route back.
    pub fn target(&self, source: SocketAddrV4) -> SocketAddrV4 {
        if self.is_route_back() {
            source
        } else {
            self.0
        }
    }
}

/// What the server tells about itself in SEARCH_RESPONSE and
/// DESCRIPTION_RESPONSE.
#[derive(Debug, Clone)]
pub struct Description {
    pub individual_address: u16,
    pub serial: [u8; 6],
    pub mac: [u8; 6],
    pub name: String,
}

impl Description {
    /// Device information DIB (`03_08_02 §7.5.4.2`), 54 octets.
    pub fn device_info_dib(&self) -> Vec<u8> {
        let mut d = vec![54, 0x01, 0x02 /* TP1 */, 0x00 /* not in programming mode */];
        d.extend_from_slice(&self.individual_address.to_be_bytes());
        d.extend_from_slice(&[0, 0]); // project-installation identifier
        d.extend_from_slice(&self.serial);
        d.extend_from_slice(&[0, 0, 0, 0]); // no routing
        d.extend_from_slice(&self.mac);
        let mut name = [0u8; 30];
        for (n, c) in name.iter_mut().zip(self.name.bytes().take(29)) {
            *n = c;
        }
        d.extend_from_slice(&name);
        d
    }

    /// Supported service families DIB (`03_08_02 §7.5.4.3`): Core,
    /// Device Management and Tunnelling, version 1.
    pub fn service_families_dib(&self) -> Vec<u8> {
        vec![8, 0x02, 0x02, 1, 0x03, 1, 0x04, 1]
    }

    pub fn dibs(&self) -> Vec<u8> {
        let mut d = self.device_info_dib();
        d.extend(self.service_families_dib());
        d
    }
}

pub fn search_response(control: Hpai, desc: &Description) -> Vec<u8> {
    let mut body = control.encode().to_vec();
    body.extend(desc.dibs());
    frame(service::SEARCH_RESPONSE, &body)
}

pub fn description_response(desc: &Description) -> Vec<u8> {
    frame(service::DESCRIPTION_RESPONSE, &desc.dibs())
}

/// CONNECT_RESPONSE; `data` and `crd` only on success.
pub fn connect_response(channel: u8, status: u8, data: Option<(Hpai, &[u8])>) -> Vec<u8> {
    let mut body = vec![channel, status];
    if let Some((hpai, crd)) = data {
        body.extend_from_slice(&hpai.encode());
        body.extend_from_slice(crd);
    }
    frame(service::CONNECT_RESPONSE, &body)
}

/// CONNECT_REQUEST for a tunnel on the data link layer (basic CRI,
/// `03_08_04 §5.4.3.1`).
pub fn connect_request_tunnel(control: Hpai, data: Hpai) -> Vec<u8> {
    let mut body = control.encode().to_vec();
    body.extend_from_slice(&data.encode());
    body.extend_from_slice(&[4, TUNNEL_CONNECTION, TUNNEL_LINKLAYER, 0]);
    frame(service::CONNECT_REQUEST, &body)
}

pub fn connectionstate_request(channel: u8, control: Hpai) -> Vec<u8> {
    let mut body = vec![channel, 0];
    body.extend_from_slice(&control.encode());
    frame(service::CONNECTIONSTATE_REQUEST, &body)
}

pub fn connectionstate_response(channel: u8, status: u8) -> Vec<u8> {
    frame(service::CONNECTIONSTATE_RESPONSE, &[channel, status])
}

pub fn disconnect_request(channel: u8, control: Hpai) -> Vec<u8> {
    let mut body = vec![channel, 0];
    body.extend_from_slice(&control.encode());
    frame(service::DISCONNECT_REQUEST, &body)
}

pub fn disconnect_response(channel: u8, status: u8) -> Vec<u8> {
    frame(service::DISCONNECT_RESPONSE, &[channel, status])
}

/// TUNNELLING_REQUEST or DEVICE_CONFIGURATION_REQUEST: connection header
/// (`03_08_04 §5.4.5`) followed by a cEMI frame.
pub fn data_request(service: u16, channel: u8, seq: u8, cemi: &[u8]) -> Vec<u8> {
    let mut body = vec![4, channel, seq, 0];
    body.extend_from_slice(cemi);
    frame(service, &body)
}

/// TUNNELLING_ACK or DEVICE_CONFIGURATION_ACK.
pub fn data_ack(service: u16, channel: u8, seq: u8, status: u8) -> Vec<u8> {
    frame(service, &[4, channel, seq, status])
}

/// The connection header of a received data request or ack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionHeader {
    pub channel: u8,
    pub seq: u8,
    /// Reserved octet in requests, status in acks.
    pub status: u8,
}

pub fn parse_connection_header(body: &[u8]) -> Result<(ConnectionHeader, &[u8])> {
    if body.len() < 4 || body[0] != 4 {
        bail!("bad connection header: {body:02X?}");
    }
    Ok((ConnectionHeader { channel: body[1], seq: body[2], status: body[3] }, &body[4..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_request_example() {
        // `03_08_02 §8.8.1`: SEARCH_REQUEST from 192.168.200.12:3671.
        let f = [0x06, 0x10, 0x02, 0x01, 0x00, 0x0E, 0x08, 0x01, 192, 168, 200, 12, 0x0E, 0x57];
        let (svc, body) = parse(&f).unwrap();
        assert_eq!(svc, service::SEARCH_REQUEST);
        let (hpai, rest) = Hpai::parse(body).unwrap();
        assert_eq!(hpai.0, "192.168.200.12:3671".parse().unwrap());
        assert!(rest.is_empty());
        assert_eq!(frame(service::SEARCH_REQUEST, &hpai.encode()), f);
    }

    #[test]
    fn route_back() {
        let (h, _) = Hpai::parse(&[8, 1, 0, 0, 0, 0, 0, 0]).unwrap();
        let src: SocketAddrV4 = "10.0.2.15:50000".parse().unwrap();
        assert_eq!(h.target(src), src);
        assert!(Hpai::parse(&[8, 1, 0, 0, 0, 0, 0x0E, 0x57]).is_err());
        assert!(Hpai::parse(&[8, 2, 0, 0, 0, 0, 0, 0]).is_err());
    }

    #[test]
    fn bad_length() {
        assert!(parse(&[0x06, 0x10, 0x02, 0x01, 0x00, 0x0F, 0, 0, 0, 0, 0, 0, 0, 0]).is_err());
    }

    #[test]
    fn dibs() {
        let d = Description {
            individual_address: 0x11FA,
            serial: [0, 0xFA, 1, 2, 3, 4],
            mac: [2, 0, 0, 0, 0, 1],
            name: "knx-sim".into(),
        };
        let dib = d.device_info_dib();
        assert_eq!(dib.len(), 54);
        assert_eq!(&dib[..6], &[54, 1, 2, 0, 0x11, 0xFA]);
        assert_eq!(&dib[24..31], b"knx-sim");
        assert_eq!(d.dibs().len(), 62);
        let f = data_request(service::TUNNELLING_REQUEST, 7, 3, &[0x29, 0]);
        let (svc, body) = parse(&f).unwrap();
        assert_eq!(svc, service::TUNNELLING_REQUEST);
        let (h, cemi) = parse_connection_header(body).unwrap();
        assert_eq!((h.channel, h.seq, cemi), (7, 3, &[0x29u8, 0][..]));
    }
}
