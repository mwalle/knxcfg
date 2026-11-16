//! Group Object Diagnostics (`03_05_01 §4.8.1`, `03_05_02 §5.1
//! DMP_GO_DIAGNOSTICS_RCl`): the function property `PID_GO_DIAGNOSTICS` of
//! the Group Object Table object gives point-to-point access to a group
//! object's configuration and value, whether or not a group address is
//! linked to it.
//!
//! Only the two read services are implemented; both are available in the
//! Normal Operation Mode, so the Operation Mode is left alone:
//!
//! * ReadServiceID 00h, Get GO Config (`§4.8.1.4.2`) → `E_GD_CONFIG`.
//! * ReadServiceID 01h, Get local GO value (`§4.8.1.4.3`) →
//!   `E_GD_GO_STATUS_VALUE`. It modifies no communication flag and is not
//!   subject to the config flags.
//!
//! GO Diagnostics is an optional Profile Module (`06 Profiles §9.2`),
//! mandatory only with KNX Data Security; a device without it has no
//! `PID_GO_DIAGNOSTICS`.

use super::mgmt::{self, Connection};
use anyhow::{bail, Result};

/// Interface object type of the Group Object Table (`03_05_01 §4.8`).
pub const OT_GROUP_OBJECT_TABLE: u16 = 9;
/// `PID_GO_DIAGNOSTICS` (`03_05_01 §4.8.1`).
pub const PID_GO_DIAGNOSTICS: u8 = 66;

/// ReadServiceIDs (`03_05_01 Table 41`).
pub const GET_GO_CONFIG: u8 = 0x00;
pub const GET_LOCAL_GO_VALUE: u8 = 0x01;

/// Positive return codes (`03_05_01 §4.8.1.1.6`).
const E_GD_CONFIG: u8 = 0x20;
const E_GD_GO_STATUS_VALUE: u8 = 0x21;

/// `GO Config` of `E_GD_CONFIG` (`03_05_01 Figure 23`): bits 7..0 are the
/// high octet of the System B group object descriptor (`Table 87`), then
/// `auth` (8), `conf` (9) and `L` (10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoConfig {
    pub config: u16,
    /// Value field type (`03_05_01 Table 88`).
    pub value_field_type: u8,
    /// Datapoint type (main, sub); `None` when the device returns 0
    /// (unknown or not supported).
    pub dpt: Option<(u16, u16)>,
}

impl GoConfig {
    pub const UPDATE: u16 = 1 << 7;
    pub const TRANSMIT: u16 = 1 << 6;
    pub const READ_ON_INIT: u16 = 1 << 5;
    pub const WRITE: u16 = 1 << 4;
    pub const READ: u16 = 1 << 3;
    pub const COMMUNICATION: u16 = 1 << 2;
    pub const AUTH: u16 = 1 << 8;
    pub const CONF: u16 = 1 << 9;
    pub const LINKED: u16 = 1 << 10;

    pub fn has(&self, flag: u16) -> bool {
        self.config & flag != 0
    }

    /// Transmission priority bits (`Table 87`: 00 system, 01 normal,
    /// 10 urgent, 11 low).
    pub fn priority(&self) -> u8 {
        (self.config & 0x03) as u8
    }
}

/// `E_GD_GO_STATUS_VALUE` (`03_05_01 Figure 24`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoValue {
    /// `GO Status` (`Figure 25`): update (3), data request (2),
    /// transmission status (1..0), coded as the RAM-flags of
    /// `§4.18.3.1.1.1`.
    pub status: u8,
    /// The value, right aligned: unused leading bits are 0.
    pub value: Vec<u8>,
}

/// The request data: `[reserved 00h][ReadServiceID][GO number:2]`.
pub fn read_request(service: u8, go: u16) -> Vec<u8> {
    let [hi, lo] = go.to_be_bytes();
    vec![0x00, service, hi, lo]
}

/// Name of a GO Diagnostics return code (`03_05_01 §4.8.1.1.6/7`).
pub fn return_code_name(code: u8) -> &'static str {
    match code {
        0x00 => "E_SUCCESS",
        0x20 => "E_GD_CONFIG",
        0x21 => "E_GD_GO_STATUS_VALUE",
        0xA1 => "E_GD_GO_VOID: the device has no group object with this number",
        0xA2 => "E_GD_CONFIG_FLAGS: the config flags do not allow the request",
        0xA3 => "E_GD_GO_SIZE_MISMATCH",
        0xF2 => "E_COMMAND_INVALID: the device does not support this command",
        0xF3 => "E_COMMAND_IMPOSSIBLE: not in the current operation mode",
        0xFF => "E_ERROR",
        _ => "unknown return code",
    }
}

/// Check the common response header `[return code][ServiceID][GO number:2]`
/// for the positive code `expected`; returns the octets after it.
fn check_response<'a>(resp: &'a [u8], service: u8, go: u16, expected: u8) -> Result<&'a [u8]> {
    let Some(&code) = resp.first() else {
        bail!("the device does not support Group Object Diagnostics (no PID_GO_DIAGNOSTICS)");
    };
    if code != expected {
        bail!("group object {go}: return code {code:02X}h ({})", return_code_name(code));
    }
    if resp.len() < 4 {
        bail!("GO Diagnostics response too short: {resp:02X?}");
    }
    if resp[1] != service {
        bail!("GO Diagnostics response for ReadServiceID {:02X}h, expected {service:02X}h", resp[1]);
    }
    let got = u16::from_be_bytes([resp[2], resp[3]]);
    if got != go {
        bail!("GO Diagnostics response for group object {got}, expected {go}");
    }
    Ok(&resp[4..])
}

/// Parse `E_GD_CONFIG`: `[20h][00h][GO:2][GO Config:2][Size:1][DPT main:2][DPT sub:2]`.
pub fn parse_config(resp: &[u8], go: u16) -> Result<GoConfig> {
    let r = check_response(resp, GET_GO_CONFIG, go, E_GD_CONFIG)?;
    if r.len() < 7 {
        bail!("E_GD_CONFIG too short: {resp:02X?}");
    }
    let main = u16::from_be_bytes([r[3], r[4]]);
    let sub = u16::from_be_bytes([r[5], r[6]]);
    Ok(GoConfig {
        config: u16::from_be_bytes([r[0], r[1]]),
        value_field_type: r[2],
        dpt: (main != 0 || sub != 0).then_some((main, sub)),
    })
}

/// Parse `E_GD_GO_STATUS_VALUE`: `[21h][01h][GO:2][GO Status:1][value]`.
pub fn parse_value(resp: &[u8], go: u16) -> Result<GoValue> {
    let r = check_response(resp, GET_LOCAL_GO_VALUE, go, E_GD_GO_STATUS_VALUE)?;
    let Some((&status, value)) = r.split_first() else {
        bail!("E_GD_GO_STATUS_VALUE without GO Status: {resp:02X?}");
    };
    if value.is_empty() {
        bail!("E_GD_GO_STATUS_VALUE without a value: {resp:02X?}");
    }
    Ok(GoValue { status, value: value.to_vec() })
}

/// ReadServiceID 00h, Get GO Config, on the Group Object Table object at
/// `object_index`.
pub fn get_config(c: &mut Connection, object_index: u8, go: u16) -> Result<GoConfig> {
    let resp = mgmt::function_property_state_read(c, object_index, PID_GO_DIAGNOSTICS, &read_request(GET_GO_CONFIG, go))?;
    parse_config(&resp, go)
}

/// ReadServiceID 01h, Get local GO value.
pub fn get_value(c: &mut Connection, object_index: u8, go: u16) -> Result<GoValue> {
    let resp =
        mgmt::function_property_state_read(c, object_index, PID_GO_DIAGNOSTICS, &read_request(GET_LOCAL_GO_VALUE, go))?;
    parse_value(&resp, go)
}

/// Size of a value field type (`03_05_01 Table 88`): (octets, bits), bits
/// set only for the types shorter than one octet.
pub fn value_field_size(vft: u8) -> (usize, Option<u8>) {
    match vft {
        0..=6 => (1, Some(vft + 1)),
        7..=10 => (vft as usize - 6, None),
        11 => (6, None),
        12 => (8, None),
        13 => (10, None),
        14 => (14, None),
        15 => (5, None),
        16 => (7, None),
        17 => (9, None),
        18..=20 => (vft as usize - 7, None),
        255 => (252, None),
        n => (n as usize - 6, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_layout() {
        assert_eq!(read_request(GET_LOCAL_GO_VALUE, 0x0123), vec![0x00, 0x01, 0x01, 0x23]);
    }

    #[test]
    fn config_parsing() {
        // GO 7: linked, C R W T U, low priority, 2 octets, DPT 9.001.
        let resp = [0x20, 0x00, 0x00, 0x07, 0x04, 0xDF, 0x08, 0x00, 0x09, 0x00, 0x01];
        let c = parse_config(&resp, 7).unwrap();
        assert!(c.has(GoConfig::LINKED) && c.has(GoConfig::UPDATE) && !c.has(GoConfig::READ_ON_INIT));
        assert_eq!(c.priority(), 3);
        assert_eq!(c.value_field_type, 8);
        assert_eq!(c.dpt, Some((9, 1)));
        let unknown = [0x20, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0, 0, 0, 0];
        assert_eq!(parse_config(&unknown, 7).unwrap().dpt, None);
        assert!(parse_config(&resp, 8).is_err(), "other object");
    }

    #[test]
    fn value_parsing() {
        let v = parse_value(&[0x21, 0x01, 0x00, 0x02, 0x08, 0x0C, 0x1A], 2).unwrap();
        assert_eq!(v, GoValue { status: 0x08, value: vec![0x0C, 0x1A] });
        let err = parse_value(&[0xA1, 0x01], 2).unwrap_err().to_string();
        assert!(err.contains("E_GD_GO_VOID"), "{err}");
        let err = parse_value(&[], 2).unwrap_err().to_string();
        assert!(err.contains("does not support"), "{err}");
    }

    #[test]
    fn value_field_sizes() {
        assert_eq!(value_field_size(0), (1, Some(1)));
        assert_eq!(value_field_size(6), (1, Some(7)));
        assert_eq!(value_field_size(7), (1, None));
        assert_eq!(value_field_size(10), (4, None));
        assert_eq!(value_field_size(11), (6, None));
        assert_eq!(value_field_size(14), (14, None));
        assert_eq!(value_field_size(15), (5, None));
        assert_eq!(value_field_size(20), (13, None));
        assert_eq!(value_field_size(21), (15, None));
        assert_eq!(value_field_size(254), (248, None));
        assert_eq!(value_field_size(255), (252, None));
    }
}
