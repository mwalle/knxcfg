//! Application-layer management APDUs (the `A_*` services).
//!
//! These are the *TSDU* (payload) octets that ride inside a cEMI `L_Data`
//! NPDU, behind the one-octet Transport Control Field (TPCI). They are the
//! message formats ETS uses to program a device (download), and are fully
//! specified by the published KNX standard — see `docs/spec-index.md`:
//!
//! - `03_03_07 §3 Application Layer` — APCI codes + PDU field layouts
//!   (`Table 1`, and the per-service figures).
//! - `03_05_03 §3.5 Load procedures` — which `A_*` services make up a download
//!   (`Load Controls` table, `DMP_LoadStateMachineWrite`, `DMP_MemWrite`).
//!
//! ## APCI encoding
//!
//! ## APCI encoding
//!
//! The Application Layer Control Information (APCI) is 10 bits wide
//! (`03_03_07 Table 1`); services with a "4-bit" APCI use only its top four
//! bits and carry data in the low six:
//!
//! | service                     | APCI (binary)  |
//! |-----------------------------|----------------|
//! | `A_Memory_Read`             | `1000nnnnnn`   |
//! | `A_Memory_Write`            | `1010nnnnnn`   |
//! | `A_IndividualAddress_Write` | `0011000000`   |
//! | `A_DeviceDescriptor_Read`   | `1100dddddd`   |
//! | `A_Restart`                 | `1110000000`   |
//! | `A_PropertyValue_Read`      | `1111010101`   |
//! | `A_PropertyValue_Write`     | `1111010111`   |
//!
//! On the wire the APCI shares its first octet with the Transport Layer
//! control field: octet 6 of the frame is `TPCI (6 bits) | APCI bits 9..8`,
//! octet 7 is `APCI bits 7..0` (`03_03_07 Figure 46`). The functions here
//! return the TPDU **with the TPCI bits left zero**: octet 0 holds only the
//! two high APCI bits, and [`crate::cemi::l_data_req`] ORs the TPCI in.
//!
//! > NOTE: these layouts are implemented from the published figures. Exact
//! > byte-level agreement with ETS is verified in the M4/M5 integration step
//! > (the conformance-test oracle); see `docs/plan.md` §9.

/// 10-bit APCI codes (`03_03_07 Table 1`).
pub mod apci {
    pub const MEMORY_READ: u16 = 0x200;
    pub const MEMORY_WRITE: u16 = 0x280;
    pub const INDIVIDUAL_ADDRESS_WRITE: u16 = 0x0C0;
    pub const DEVICE_DESCRIPTOR_READ: u16 = 0x300;
    pub const RESTART: u16 = 0x380;
    pub const PROPERTY_VALUE_READ: u16 = 0x3D5;
    pub const PROPERTY_VALUE_WRITE: u16 = 0x3D7;

    /// `PID_LOAD_STATE_CONTROL` (`03_05_01 §4.2.5`).
    pub const PID_LOAD_STATE_CONTROL: u8 = 5;
}

/// The two APCI octets: `[APCI bits 9..8][APCI bits 7..6 | field]`.
fn apci_octets(apci: u16, field: u8) -> [u8; 2] {
    [((apci >> 8) & 0x03) as u8, (apci as u8 & 0xC0) | (apci as u8 & 0x3F) | (field & 0x3F)]
}

/// `A_Memory_Write` (`03_03_07 §3.5.4`): `[..10][10nnnnnn][addr:2][data]`,
/// `nnnnnn` = number of octets (1..=12 in a standard frame).
pub fn memory_write(address: u16, data: &[u8]) -> Vec<u8> {
    assert!(
        !data.is_empty() && data.len() <= 63,
        "A_Memory_Write carries 1..=63 octets"
    );
    let mut out = Vec::with_capacity(4 + data.len());
    out.extend_from_slice(&apci_octets(apci::MEMORY_WRITE, data.len() as u8));
    out.extend_from_slice(&address.to_be_bytes());
    out.extend_from_slice(data);
    out
}

/// `A_Memory_Read` (`03_03_07 §3.5.3`): `[..10][00nnnnnn][addr:2]`.
pub fn memory_read(address: u16, count: u8) -> Vec<u8> {
    assert!((1..=63).contains(&count), "A_Memory_Read reads 1..=63 octets");
    let mut out = apci_octets(apci::MEMORY_READ, count).to_vec();
    out.extend_from_slice(&address.to_be_bytes());
    out
}

/// `A_DeviceDescriptor_Read` (`03_03_07 §3.4.2.1`); type 0 = DD0.
pub fn device_descriptor_read(descriptor_type: u8) -> Vec<u8> {
    apci_octets(apci::DEVICE_DESCRIPTOR_READ, descriptor_type).to_vec()
}

/// `A_Restart` (`03_03_07 §3.4.2.2`), basic restart.
pub fn restart() -> Vec<u8> {
    apci_octets(apci::RESTART, 0).to_vec()
}

/// `A_PropertyValue_Write` (`03_03_07 §3.4.4.2`):
/// `[APCI:2][object_index][property_id][nr_of_elem:4 | start_index:12][data]`.
/// Array elements start at index 1 (index 0 is the current element count).
pub fn property_value_write(
    object_index: u8,
    property_id: u8,
    nr_of_elem: u8,
    start_index: u16,
    data: &[u8],
) -> Vec<u8> {
    let mut out = property_header(apci::PROPERTY_VALUE_WRITE, object_index, property_id, nr_of_elem, start_index);
    out.extend_from_slice(data);
    out
}

/// `A_PropertyValue_Read` (`03_03_07 §3.4.4.1`).
pub fn property_value_read(object_index: u8, property_id: u8, nr_of_elem: u8, start_index: u16) -> Vec<u8> {
    property_header(apci::PROPERTY_VALUE_READ, object_index, property_id, nr_of_elem, start_index)
}

fn property_header(apci: u16, object_index: u8, property_id: u8, nr_of_elem: u8, start_index: u16) -> Vec<u8> {
    assert!(nr_of_elem <= 15 && start_index <= 0x0FFF);
    let mut out = apci_octets(apci, 0).to_vec();
    out.push(object_index);
    out.push(property_id);
    out.push((nr_of_elem << 4) | (start_index >> 8) as u8);
    out.push(start_index as u8);
    out
}

/// `A_IndividualAddress_Write` (`03_03_07 §3.2.2`), broadcast in programming
/// mode: `[APCI:2][new address:2]`.
pub fn individual_address_write(new_address: u16) -> Vec<u8> {
    let mut out = apci_octets(apci::INDIVIDUAL_ADDRESS_WRITE, 0).to_vec();
    out.extend_from_slice(&new_address.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_write_layout() {
        // A_Memory_Write(addr=0x0200, 3 octets): APCI 1010 000011.
        assert_eq!(
            memory_write(0x0200, &[0xAA, 0xBB, 0xCC]),
            vec![0x02, 0x83, 0x02, 0x00, 0xAA, 0xBB, 0xCC]
        );
    }

    #[test]
    fn memory_read_layout() {
        assert_eq!(memory_read(0x0200, 5), vec![0x02, 0x05, 0x02, 0x00]);
    }

    #[test]
    fn device_descriptor_read_layout() {
        assert_eq!(device_descriptor_read(0), vec![0x03, 0x00]);
    }

    #[test]
    fn restart_layout() {
        assert_eq!(restart(), vec![0x03, 0x80]);
    }

    #[test]
    fn property_value_write_load_control() {
        // Unload LSM 1: object 1, PID 5, 1 element at index 1, event 04h.
        let pdu = property_value_write(1, apci::PID_LOAD_STATE_CONTROL, 1, 1, &[0x04]);
        assert_eq!(pdu, vec![0x03, 0xD7, 0x01, 0x05, 0x10, 0x01, 0x04]);
        assert_eq!(property_value_read(0, 78, 1, 1), vec![0x03, 0xD5, 0x00, 0x4E, 0x10, 0x01]);
    }

    #[test]
    fn individual_address_write_layout() {
        assert_eq!(individual_address_write(0x1101), vec![0x00, 0xC0, 0x11, 0x01]);
    }
}
