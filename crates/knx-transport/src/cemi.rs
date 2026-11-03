//! cEMI `L_Data.req` frame construction for device management (M4/M5).
//!
//! The frame layout is `03_06_03 §4.1.5.3.3 L_Data.req`:
//!
//! ```text
//! MC=11h  AddIL  Ctrl1  Ctrl2  SAH SAL  DAH DAL  L  TPCI  APDU…
//! ```
//!
//! * `Ctrl1` / `Ctrl2` are the Data Link Layer control fields
//!   (`03_06_03 §4.1.5.3.2`).
//! * `TPCI` is the Transport Layer control field (`03_03_04 §2`); it shares
//!   its octet with the two high APCI bits.
//! * `L` is the number of TPDU octets after that first octet.
//!
//! The `A_*` management services carried in the APDU are encoded in
//! [`crate::apdu`].

use super::{message_code, CemiFrame};
use anyhow::{bail, Result};

/// Transport Control Field (TPCI) service codes (`03_03_04 Table 1`).
///
/// Bit 7 = control (1) / data (0), bit 6 = numbered, bits 5..2 = sequence
/// number, bits 1..0 = control code (data PDUs: the two high APCI bits).
pub mod tpci {
    /// Connectionless group/broadcast data (`T_Data_Group`/`T_Data_Broadcast`).
    pub const T_DATA_GROUP: u8 = 0x00;
    /// Connectionless point-to-point data (`T_Data_Individual`).
    pub const T_DATA_INDIVIDUAL: u8 = 0x00;
    /// Connection-oriented data (`T_Data_Connected`); OR in [`seq`].
    pub const T_DATA_CONNECTED: u8 = 0x40;
    /// Connection request (`T_Connect`).
    pub const T_CONNECT: u8 = 0x80;
    /// Connection release (`T_Disconnect`).
    pub const T_DISCONNECT: u8 = 0x81;
    /// Positive acknowledge (`T_ACK`); OR in [`seq`].
    pub const T_ACK: u8 = 0xC2;
    /// Negative acknowledge (`T_NAK`); OR in [`seq`].
    pub const T_NAK: u8 = 0xC3;

    /// Sequence number field (bits 5..2) of a numbered TPDU.
    pub const fn seq(n: u8) -> u8 {
        (n & 0x0F) << 2
    }
}

/// Transmission priority (`03_06_03 §4.1.5.3.2`, `Ctrl1` bits 3..=2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    System,
    Urgent,
    Normal,
    Low,
}

impl Priority {
    /// The 2-bit priority code, per the KNX TP1 control-field encoding.
    pub fn code(self) -> u8 {
        match self {
            Priority::System => 0b00,
            Priority::Urgent => 0b10,
            Priority::Normal => 0b01,
            Priority::Low => 0b11,
        }
    }
}

/// Build a cEMI `L_Data.req` frame carrying one management APDU.
///
/// `src` is the source Individual Address (`0` lets the cEMI server insert its
/// own), `dst` the destination Individual Address, `apdu` the
/// application-layer PDU as built by [`crate::apdu`] (its first octet holds
/// only the two high APCI bits; the TPCI is ORed in). An empty `apdu` gives a
/// transport control frame (`T_Connect`, `T_Disconnect`, `T_ACK`).
pub fn l_data_req(tpci: u8, src: u16, dst: u16, apdu: &[u8]) -> CemiFrame {
    // Ctrl1: standard frame (FT=1), no repetition on error (R=1; R=0 would
    // request repetitions, `03_06_03 §4.1.5.3.2`), broadcast (not
    // system-broadcast, SB=1), system priority, L2 acknowledge requested.
    let ctrl1 = 0x80 | 0x20 | 0x10 | (Priority::System.code() << 2) | 0x02;
    // Ctrl2: individual destination (AT=0), hop count 6, standard frame
    // (EFF=0000).
    let ctrl2 = 0x60;

    let mut body = Vec::with_capacity(7 + 1 + apdu.len());
    body.push(ctrl1);
    body.push(ctrl2);
    body.push((src >> 8) as u8);
    body.push(src as u8);
    body.push((dst >> 8) as u8);
    body.push(dst as u8);
    match apdu.split_first() {
        Some((first, rest)) => {
            body.push(rest.len() as u8); // L = octets after the TPCI/APCI octet
            body.push(tpci | (first & 0x03));
            body.extend_from_slice(rest);
        }
        None => {
            body.push(0);
            body.push(tpci);
        }
    }

    CemiFrame {
        message_code: message_code::L_DATA_REQ,
        additional_info: Vec::new(),
        body,
    }
}

/// `L_Data.req` broadcast (`T_Data_Broadcast`, destination 0/0/0 with
/// address type group, `03_03_04 §2`) carrying `apdu`.
pub fn l_data_broadcast(src: u16, apdu: &[u8]) -> CemiFrame {
    let mut f = l_data_req(tpci::T_DATA_GROUP, src, 0x0000, apdu);
    f.body[1] |= 0x80; // Ctrl2 AT = group
    f
}

/// `T_Connect` / `T_Disconnect` control frame (no APDU).
pub fn l_data_control(tpci: u8, src: u16, dst: u16) -> CemiFrame {
    l_data_req(tpci, src, dst, &[])
}

/// A received cEMI `L_Data` frame (`L_Data.ind` / `L_Data.con`,
/// `03_06_03 §4.1.5.3`), split into its control fields, addresses and TPDU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LData {
    pub message_code: u8,
    pub ctrl1: u8,
    pub ctrl2: u8,
    pub src: u16,
    pub dst: u16,
    /// TPCI/APCI octet followed by the `L` remaining TPDU octets.
    pub tpdu: Vec<u8>,
}

impl LData {
    pub fn parse(frame: &CemiFrame) -> Result<Self> {
        let b = &frame.body;
        if b.len() < 8 {
            bail!("cEMI L_Data frame too short: {} octets", b.len());
        }
        let len = b[6] as usize;
        if b.len() < 8 + len {
            bail!("cEMI L_Data frame truncated: L={len}, {} TPDU octets", b.len() - 7);
        }
        Ok(Self {
            message_code: frame.message_code,
            ctrl1: b[0],
            ctrl2: b[1],
            src: u16::from_be_bytes([b[2], b[3]]),
            dst: u16::from_be_bytes([b[4], b[5]]),
            tpdu: b[7..8 + len].to_vec(),
        })
    }

    /// Destination is a group address (`Ctrl2` bit 7, address type).
    pub fn is_group(&self) -> bool {
        self.ctrl2 & 0x80 != 0
    }

    /// Negative confirmation: Confirm flag set in `Ctrl1`
    /// (`03_06_03 §4.1.5.3.4 L_Data.con`).
    pub fn is_negative_confirm(&self) -> bool {
        self.ctrl1 & 0x01 != 0
    }

    /// The TPCI: the whole first octet for control PDUs (`T_Connect`,
    /// `T_ACK`, …), the six high bits for data PDUs.
    pub fn tpci(&self) -> u8 {
        if self.tpdu[0] & 0x80 != 0 {
            self.tpdu[0]
        } else {
            self.tpdu[0] & 0xFC
        }
    }

    /// Sequence number of a numbered TPDU (`03_03_04 §2`, bits 5..2).
    pub fn seq(&self) -> u8 {
        (self.tpdu[0] >> 2) & 0x0F
    }

    /// The APDU of a data PDU in the [`crate::apdu`] convention (TPCI bits
    /// cleared); `None` for control PDUs.
    pub fn apdu(&self) -> Option<Vec<u8>> {
        if self.tpdu[0] & 0x80 != 0 {
            return None;
        }
        let mut apdu = self.tpdu.clone();
        apdu[0] &= 0x03;
        Some(apdu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l_data_req_layout() {
        // A_DeviceDescriptor_Read(0), connectionless.
        let f = l_data_req(tpci::T_DATA_INDIVIDUAL, 0, 0x1001, &crate::apdu::device_descriptor_read(0));
        let bytes = f.encode();
        // MC, AddIL=0, Ctrl1, Ctrl2, src 0x0000, dst 0x1001, L=1, TPCI|APCI, APCI.
        assert_eq!(
            bytes,
            vec![0x11, 0x00, 0xB2, 0x60, 0x00, 0x00, 0x10, 0x01, 0x01, 0x03, 0x00]
        );
        // T_Connect: L=0, TPCI 80h.
        let c = l_data_control(tpci::T_CONNECT, 0, 0x1001).encode();
        assert_eq!(&c[8..], &[0x00, 0x80]);
        // A_Restart on connection sequence 3: TPCI 01 0011 + APCI 11.
        let r = l_data_req(tpci::T_DATA_CONNECTED | tpci::seq(3), 0, 0x1001, &crate::apdu::restart());
        assert_eq!(&r.encode()[8..], &[0x01, 0x4F, 0x80]);
    }

    #[test]
    fn l_data_parse() {
        // A_Memory_Response from 1.1.1, connected seq 2, as an L_Data.ind.
        let mut f = l_data_req(tpci::T_DATA_CONNECTED | tpci::seq(2), 0x1101, 0x0000, &[0x02, 0x42, 0x01, 0x00, 0xAB, 0xCD]);
        f.message_code = message_code::L_DATA_IND;
        let ld = LData::parse(&f).unwrap();
        assert_eq!((ld.src, ld.dst, ld.tpci(), ld.seq()), (0x1101, 0, tpci::T_DATA_CONNECTED | tpci::seq(2), 2));
        assert!(!ld.is_group() && !ld.is_negative_confirm());
        assert_eq!(ld.apdu().unwrap(), vec![0x02, 0x42, 0x01, 0x00, 0xAB, 0xCD]);
        let ack = LData::parse(&l_data_control(tpci::T_ACK | tpci::seq(5), 0x1101, 0)).unwrap();
        assert_eq!((ack.tpci(), ack.seq(), ack.apdu()), (tpci::T_ACK | tpci::seq(5), 5, None));
        let mut short = f.clone();
        short.body.truncate(9);
        assert!(LData::parse(&short).is_err());
    }

    #[test]
    fn connected_sequence_in_tpci() {
        assert_eq!(tpci::T_DATA_CONNECTED | tpci::seq(3), 0x4C);
        assert_eq!(tpci::T_ACK | tpci::seq(15), 0xFE);
    }
}
