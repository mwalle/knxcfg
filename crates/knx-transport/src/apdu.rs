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
//! The Application Layer Control Information (APCI) is 10 bits wide
//! (`03_03_07 Table 1`); services with a "4-bit" APCI use only its top four
//! bits and carry data in the low six:
//!
//! | service                     | APCI (binary)  |
//! |-----------------------------|----------------|
//! | `A_Memory_Read`             | `1000nnnnnn`   |
//! | `A_Memory_Write`            | `1010nnnnnn`   |
//! | `A_IndividualAddress_Write` | `0011000000`   |
//! | `A_IndividualAddress_Read`  | `0100000000`   |
//! | `A_IndividualAddress_Response` | `0101000000` |
//! | `A_DeviceDescriptor_Read`   | `1100dddddd`   |
//! | `A_Restart`                 | `1110000000`   |
//! | `A_PropertyValue_Read`      | `1111010101`   |
//! | `A_PropertyValue_Write`     | `1111010111`   |
//! | `A_IndividualAddressSerialNumber_Read`     | `1111011100` |
//! | `A_IndividualAddressSerialNumber_Response` | `1111011101` |
//! | `A_IndividualAddressSerialNumber_Write`    | `1111011110` |
//! | `A_FunctionPropertyCommand` | `1011000111`   |
//! | `A_FilterTable_Open`        | `1111000000`   |
//! | `A_FilterTable_Write`       | `1111000011`   |
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

use anyhow::{bail, Result};

/// 10-bit APCI codes (`03_03_07 Table 1`).
pub mod apci {
    pub const MEMORY_READ: u16 = 0x200;
    pub const MEMORY_RESPONSE: u16 = 0x240;
    pub const MEMORY_WRITE: u16 = 0x280;
    pub const INDIVIDUAL_ADDRESS_WRITE: u16 = 0x0C0;
    pub const INDIVIDUAL_ADDRESS_READ: u16 = 0x100;
    pub const INDIVIDUAL_ADDRESS_RESPONSE: u16 = 0x140;
    pub const SERIAL_NUMBER_READ: u16 = 0x3DC;
    pub const SERIAL_NUMBER_RESPONSE: u16 = 0x3DD;
    pub const SERIAL_NUMBER_WRITE: u16 = 0x3DE;
    pub const DEVICE_DESCRIPTOR_READ: u16 = 0x300;
    pub const DEVICE_DESCRIPTOR_RESPONSE: u16 = 0x340;
    pub const RESTART: u16 = 0x380;
    pub const PROPERTY_VALUE_READ: u16 = 0x3D5;
    pub const PROPERTY_VALUE_WRITE: u16 = 0x3D7;
    pub const FUNCTION_PROPERTY_COMMAND: u16 = 0x2C7;
    pub const FUNCTION_PROPERTY_STATE_RESPONSE: u16 = 0x2C9;
    pub const FILTER_TABLE_OPEN: u16 = 0x3C0;
    pub const FILTER_TABLE_WRITE: u16 = 0x3C3;

    /// `PID_ROUTETABLE_CONTROL` of the router object (`03_05_01 §4.5.7`).
    pub const PID_ROUTETABLE_CONTROL: u8 = 56;
    /// `SRVID_CLEAR_FILTERTABLE` (`03_05_01 §4.5.7.2`).
    pub const SRVID_CLEAR_FILTERTABLE: u8 = 1;

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

/// `A_FunctionPropertyCommand` (`03_03_07 §3.4.7.1`):
/// `[APCI:2][object_index][property_id][data]`.
pub fn function_property_command(object_index: u8, property_id: u8, data: &[u8]) -> Vec<u8> {
    let mut out = apci_octets(apci::FUNCTION_PROPERTY_COMMAND, 0).to_vec();
    out.push(object_index);
    out.push(property_id);
    out.extend_from_slice(data);
    out
}

/// `A_FilterTable_Open` (`03_03_07 §3.6.1`): no fields.
pub fn filter_table_open() -> Vec<u8> {
    apci_octets(apci::FILTER_TABLE_OPEN, 0).to_vec()
}

/// `A_FilterTable_Write` (`03_03_07 §3.6.3`):
/// `[APCI:2][number][filter_table_address:2][data]`, 1..=254 octets.
pub fn filter_table_write(address: u16, data: &[u8]) -> Vec<u8> {
    assert!(
        !data.is_empty() && data.len() <= 254,
        "A_FilterTable_Write carries 1..=254 octets"
    );
    let mut out = apci_octets(apci::FILTER_TABLE_WRITE, 0).to_vec();
    out.push(data.len() as u8);
    out.extend_from_slice(&address.to_be_bytes());
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

/// `A_IndividualAddress_Read` (`03_03_07 §3.2.3`), broadcast; answered by
/// the devices in programming mode.
pub fn individual_address_read() -> Vec<u8> {
    apci_octets(apci::INDIVIDUAL_ADDRESS_READ, 0).to_vec()
}

/// `A_IndividualAddressSerialNumber_Read` (`03_03_07 §3.2.4`), broadcast:
/// `[APCI:2][serial number:6]`.
pub fn serial_number_read(serial: &[u8; 6]) -> Vec<u8> {
    let mut out = apci_octets(apci::SERIAL_NUMBER_READ, 0).to_vec();
    out.extend_from_slice(serial);
    out
}

/// `A_IndividualAddressSerialNumber_Write` (`03_03_07 §3.2.5`), broadcast:
/// `[APCI:2][serial number:6][new address:2][reserved:4]`.
pub fn serial_number_write(serial: &[u8; 6], new_address: u16) -> Vec<u8> {
    let mut out = apci_octets(apci::SERIAL_NUMBER_WRITE, 0).to_vec();
    out.extend_from_slice(serial);
    out.extend_from_slice(&new_address.to_be_bytes());
    out.extend_from_slice(&[0; 4]);
    out
}

/// The full 10-bit APCI of an APDU.
pub fn apci_of(apdu: &[u8]) -> Option<u16> {
    (apdu.len() >= 2).then(|| ((apdu[0] as u16 & 0x03) << 8) | apdu[1] as u16)
}

/// `A_IndividualAddressSerialNumber_Response` (`03_03_07 §3.2.4`):
/// `[APCI:2][serial number:6][domain address:2][reserved:2]` → serial number.
pub fn parse_serial_number_response(apdu: &[u8]) -> Result<[u8; 6]> {
    if apci_of(apdu) != Some(apci::SERIAL_NUMBER_RESPONSE) || apdu.len() < 8 {
        bail!("not an A_IndividualAddressSerialNumber_Response: {apdu:02X?}");
    }
    Ok(apdu[2..8].try_into().expect("6 octets"))
}

/// Does `apdu` carry the service with the 4-bit APCI `apci` (the services
/// whose low six APCI bits are data)?
pub fn is_service4(apdu: &[u8], apci: u16) -> bool {
    apdu.len() >= 2 && (((apdu[0] as u16 & 0x03) << 8) | apdu[1] as u16) & 0x3C0 == apci & 0x3C0
}

/// `A_DeviceDescriptor_Response` (`03_03_07 §3.4.2.1`):
/// `[..11][01dddddd][descriptor]` → (descriptor type, descriptor octets).
pub fn parse_device_descriptor_response(apdu: &[u8]) -> Result<(u8, Vec<u8>)> {
    if !is_service4(apdu, apci::DEVICE_DESCRIPTOR_RESPONSE) {
        bail!("not an A_DeviceDescriptor_Response: {apdu:02X?}");
    }
    let descriptor_type = apdu[1] & 0x3F;
    if descriptor_type == 0x3F {
        bail!("the device does not support this device descriptor type");
    }
    Ok((descriptor_type, apdu[2..].to_vec()))
}

/// `A_Memory_Response` (`03_03_07 §3.5.3`): `[..10][01nnnnnn][addr:2][data]`
/// → (address, data). `nnnnnn = 0` (no data) is the negative response.
pub fn parse_memory_response(apdu: &[u8]) -> Result<(u16, Vec<u8>)> {
    if !is_service4(apdu, apci::MEMORY_RESPONSE) || apdu.len() < 4 {
        bail!("not an A_Memory_Response: {apdu:02X?}");
    }
    let count = (apdu[1] & 0x3F) as usize;
    let address = u16::from_be_bytes([apdu[2], apdu[3]]);
    if count == 0 {
        bail!("memory read at {address:04X}h refused by the device");
    }
    let data = &apdu[4..];
    if data.len() != count {
        bail!("A_Memory_Response at {address:04X}h: {count} octets announced, {} received", data.len());
    }
    Ok((address, data.to_vec()))
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
    fn response_parsing() {
        assert_eq!(parse_device_descriptor_response(&[0x03, 0x40, 0x07, 0x05]).unwrap(), (0, vec![0x07, 0x05]));
        assert!(parse_device_descriptor_response(&[0x03, 0x7F]).is_err());
        assert!(parse_device_descriptor_response(&[0x02, 0x42]).is_err());
        assert_eq!(parse_memory_response(&[0x02, 0x42, 0x01, 0x00, 0xAB, 0xCD]).unwrap(), (0x0100, vec![0xAB, 0xCD]));
        assert!(parse_memory_response(&[0x02, 0x40, 0x01, 0x00]).is_err(), "negative response");
        assert!(parse_memory_response(&[0x02, 0x43, 0x01, 0x00, 0xAB]).is_err(), "short data");
    }

    #[test]
    fn individual_address_write_layout() {
        assert_eq!(individual_address_write(0x1101), vec![0x00, 0xC0, 0x11, 0x01]);
        assert_eq!(individual_address_read(), vec![0x01, 0x00]);
        assert_eq!(apci_of(&[0x01, 0x40]), Some(apci::INDIVIDUAL_ADDRESS_RESPONSE));
    }

    #[test]
    fn serial_number_layouts() {
        let sn = [0x00, 0x83, 0x12, 0x34, 0x56, 0x78];
        assert_eq!(serial_number_read(&sn), vec![0x03, 0xDC, 0x00, 0x83, 0x12, 0x34, 0x56, 0x78]);
        assert_eq!(
            serial_number_write(&sn, 0x1107),
            vec![0x03, 0xDE, 0x00, 0x83, 0x12, 0x34, 0x56, 0x78, 0x11, 0x07, 0, 0, 0, 0]
        );
        let resp = [0x03, 0xDD, 0x00, 0x83, 0x12, 0x34, 0x56, 0x78, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(parse_serial_number_response(&resp).unwrap(), sn);
        assert!(parse_serial_number_response(&resp[..6]).is_err());
    }
}
