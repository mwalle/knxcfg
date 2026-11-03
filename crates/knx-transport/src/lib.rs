//! Transport layer: cEMI framing and the bus/transport abstraction.
//!
//! The wire path is fully specified by the KNX standard; see
//! `docs/spec-index.md`:
//! - `09_03 §3 KNX USB Interface` — USB HID transport
//! - `03_06_03 §4.1 cEMI: message format and services` — cEMI framing
//!
//! - [`usb`] — KNX USB HID framing and [`usb::UsbTransport`]
//! - [`hidraw`] — Linux `/dev/hidraw*` access and KNX interface discovery
//! - [`mgmt`] — data link confirmations and the connection-oriented
//!   Transport Layer used for device management

use anyhow::{anyhow, Result};
use std::time::Duration;

pub mod apdu;
pub mod cemi;
#[cfg(target_os = "linux")]
pub mod hidraw;
pub mod mgmt;
pub mod usb;

/// cEMI message codes (`03_06_03 §4.1.1 Message code`).
pub mod message_code {
    pub const L_RAW_REQ: u8 = 0x10;
    pub const L_DATA_REQ: u8 = 0x11;
    pub const L_DATA_IND: u8 = 0x29;
    pub const L_DATA_CON: u8 = 0x2E;
    pub const L_RAW_IND: u8 = 0x2D;
    pub const L_RAW_CON: u8 = 0x2F;
    pub const M_RESET_REQ: u8 = 0xF1;
    pub const M_RESET_IND: u8 = 0xF0;
}

/// A cEMI frame (`03_06_03 §4.1 cEMI: message format and services`):
///
/// ```text
/// Message Code         1 octet
/// Additional Info Len  1 octet
/// Additional Info      0..n octets
/// cEMI body            service specific
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CemiFrame {
    pub message_code: u8,
    pub additional_info: Vec<u8>,
    pub body: Vec<u8>,
}

impl CemiFrame {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(2 + self.additional_info.len() + self.body.len());
        out.push(self.message_code);
        out.push(self.additional_info.len() as u8);
        out.extend_from_slice(&self.additional_info);
        out.extend_from_slice(&self.body);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 2 {
            return Err(anyhow!("cEMI frame too short: {} octets", bytes.len()));
        }
        let message_code = bytes[0];
        let info_len = bytes[1] as usize;
        if bytes.len() < 2 + info_len {
            return Err(anyhow!("cEMI frame truncated in additional info"));
        }
        Ok(Self {
            message_code,
            additional_info: bytes[2..2 + info_len].to_vec(),
            body: bytes[2 + info_len..].to_vec(),
        })
    }
}

/// A full-duplex cEMI transport (USB HID, KNXnet/IP, …).
pub trait Transport {
    fn send(&mut self, frame: &CemiFrame) -> Result<()>;
    fn receive(&mut self, timeout: Duration) -> Result<CemiFrame>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cemi_roundtrip() {
        let frame = CemiFrame {
            message_code: message_code::L_DATA_REQ,
            additional_info: vec![0x01, 0x00, 0x00],
            body: vec![0xBC, 0xE0, 0x00, 0x00, 0x01],
        };
        let enc = frame.encode();
        assert_eq!(frame, CemiFrame::decode(&enc).unwrap());
    }

    #[test]
    fn cemi_rejects_short() {
        assert!(CemiFrame::decode(&[0x11]).is_err());
    }
}
