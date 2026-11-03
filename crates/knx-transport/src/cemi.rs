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
    // Ctrl1: standard frame (FT=1), repeat allowed (R=1), broadcast (not
    // system-broadcast, SB=1), system priority, acknowledge requested.
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

/// `T_Connect` / `T_Disconnect` control frame (no APDU).
pub fn l_data_control(tpci: u8, src: u16, dst: u16) -> CemiFrame {
    l_data_req(tpci, src, dst, &[])
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
    fn connected_sequence_in_tpci() {
        assert_eq!(tpci::T_DATA_CONNECTED | tpci::seq(3), 0x4C);
        assert_eq!(tpci::T_ACK | tpci::seq(15), 0xFE);
    }
}
