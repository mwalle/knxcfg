//! The simulated KNX line: routes `L_Data` frames between the tunnel and the
//! simulated devices, gives the data link confirmations and logs every frame.

use crate::device::{ia, Device, Out};
use knx_transport::apdu::apci_of;
use knx_transport::cemi::{l_data_req, tpci, LData};
use knx_transport::{message_code, CemiFrame};
use std::fs::File;
use std::io::Write;
use std::time::Instant;

pub struct Log {
    start: Instant,
    file: Option<File>,
}

impl Log {
    pub fn new(file: Option<File>) -> Self {
        Log { start: Instant::now(), file }
    }

    pub fn line(&mut self, text: &str) {
        let t = self.start.elapsed();
        let line = format!("{:5}.{:03} {text}", t.as_secs(), t.subsec_millis());
        println!("{line}");
        if let Some(f) = self.file.as_mut() {
            let _ = writeln!(f, "{line}");
        }
    }
}

pub struct Bus {
    pub devices: Vec<Device>,
    pub log: Log,
}

impl Bus {
    pub fn new(devices: Vec<Device>, log: Log) -> Self {
        Bus { devices, log }
    }

    /// An `L_Data.req` from the tunnel (source already filled in). Returns
    /// the frames for the tunnel: the `L_Data.con`, then the `L_Data.ind`s
    /// of the device responses addressed to `tunnel_ia` or broadcast.
    pub fn send(&mut self, req: &CemiFrame, tunnel_ia: u16) -> Vec<CemiFrame> {
        let ld = match LData::parse(req) {
            Ok(ld) => ld,
            Err(e) => {
                self.log.line(&format!("tunnel  bad L_Data.req: {e}: {}", hex(&req.encode())));
                return Vec::new();
            }
        };
        self.log.line(&format!("tunnel> {}", describe(&ld)));

        // A frame to an individual address is acknowledged on TP1 only if a
        // device has that address; without the acknowledgement the
        // confirmation is negative (`03_06_03 §4.1.5.3.4`).
        let acked = ld.is_group() || self.devices.iter().any(|d| d.address == ld.dst);
        let mut con = req.clone();
        con.message_code = message_code::L_DATA_CON;
        if !acked {
            con.body[0] |= 0x01;
        }
        let mut out = vec![con];
        if !acked {
            self.log.line("        no device at the destination, negative L_Data.con");
        }

        let mut sent = Vec::new();
        for d in &mut self.devices {
            let mut notes = Vec::new();
            let from = d.address;
            sent.extend(d.receive(&ld, &mut notes).into_iter().map(|o| (from, o)));
            for n in notes {
                self.log.line(&format!("        {}: {n}", ia(from)));
            }
        }
        for (from, o) in sent {
            let frame = ind(from, &o);
            let ld = LData::parse(&frame).expect("own frame");
            self.log.line(&format!("bus>    {}", describe(&ld)));
            if o.broadcast || o.dst == tunnel_ia {
                out.push(frame);
            }
        }
        out
    }
}

fn ind(src: u16, o: &Out) -> CemiFrame {
    let mut f = l_data_req(o.tpci, src, o.dst, &o.apdu);
    f.message_code = message_code::L_DATA_IND;
    // Received frames carry no acknowledge request bit.
    f.body[0] &= !0x02;
    if o.broadcast {
        f.body[1] |= 0x80;
    }
    f
}

/// One-line description of an `L_Data` frame for the log.
pub fn describe(ld: &LData) -> String {
    let dst = if ld.is_group() {
        let g = ld.dst;
        format!("{}/{}/{}", g >> 11, (g >> 8) & 0x07, g & 0xFF)
    } else {
        ia(ld.dst)
    };
    let t = ld.tpci();
    let tl = match t {
        tpci::T_CONNECT => "T_Connect".to_string(),
        tpci::T_DISCONNECT => "T_Disconnect".to_string(),
        _ if t & 0xC3 == 0xC2 => format!("T_ACK {}", ld.seq()),
        _ if t & 0xC3 == 0xC3 => format!("T_NAK {}", ld.seq()),
        _ if t & 0x40 != 0 => format!("T_Data_Connected {}", ld.seq()),
        _ if ld.is_group() && ld.dst == 0 => "T_Data_Broadcast".to_string(),
        _ if ld.is_group() => "T_Data_Group".to_string(),
        _ => "T_Data_Individual".to_string(),
    };
    let app = ld.apdu().map(|a| apdu_text(&a)).unwrap_or_default();
    format!("{} > {dst} {tl}{app}", ia(ld.src))
}

fn apdu_text(a: &[u8]) -> String {
    let Some(code) = apci_of(a) else { return format!(" {}", hex(a)) };
    let data = &a[2..];
    let low = (a[1] & 0x3F) as usize;
    let name = match code & 0x3C0 {
        0x000 => "A_GroupValue_Read",
        0x040 => "A_GroupValue_Response",
        0x080 => "A_GroupValue_Write",
        0x180 => return format!(" A_ADC_Read channel {low} {}", hex(data)),
        0x1C0 => return format!(" A_ADC_Response channel {low} {}", hex(data)),
        0x200 => return format!(" A_Memory_Read {low} @{}", hex4(data)),
        0x240 => return format!(" A_Memory_Response {low} @{} {}", hex4(data), hex(data.get(2..).unwrap_or(&[]))),
        0x280 => return format!(" A_Memory_Write {low} @{} {}", hex4(data), hex(data.get(2..).unwrap_or(&[]))),
        0x300 => return format!(" A_DeviceDescriptor_Read {low}"),
        0x340 => return format!(" A_DeviceDescriptor_Response {low} {}", hex(data)),
        0x380 => "A_Restart",
        _ => match code {
            0x0C0 => "A_IndividualAddress_Write",
            0x100 => "A_IndividualAddress_Read",
            0x140 => "A_IndividualAddress_Response",
            0x2C7 => "A_FunctionPropertyCommand",
            0x2C8 => "A_FunctionPropertyState_Read",
            0x2C9 => "A_FunctionPropertyState_Response",
            0x3C0 => "A_FilterTable_Open",
            0x3C1 => "A_FilterTable_Read",
            0x3C2 => "A_FilterTable_Response",
            0x3C3 => "A_FilterTable_Write",
            0x3D1 => "A_Authorize_Request",
            0x3D2 => "A_Authorize_Response",
            0x3D3 => "A_Key_Write",
            0x3D4 => "A_Key_Response",
            0x3D5 => return format!(" A_PropertyValue_Read {}", prop(data)),
            0x3D6 => return format!(" A_PropertyValue_Response {}", prop(data)),
            0x3D7 => return format!(" A_PropertyValue_Write {}", prop(data)),
            0x3D8 => "A_PropertyDescription_Read",
            0x3D9 => "A_PropertyDescription_Response",
            0x3DC => "A_IndividualAddressSerialNumber_Read",
            0x3DD => "A_IndividualAddressSerialNumber_Response",
            0x3DE => "A_IndividualAddressSerialNumber_Write",
            _ => return format!(" APCI {code:03X}h {}", hex(data)),
        },
    };
    if data.is_empty() {
        format!(" {name}")
    } else {
        format!(" {name} {}", hex(data))
    }
}

/// Object index, PID, count, start index and data of a property PDU.
fn prop(d: &[u8]) -> String {
    if d.len() < 4 {
        return hex(d);
    }
    let start = u16::from_be_bytes([d[2] & 0x0F, d[3]]);
    let mut s = format!("obj {} pid {} n {} start {start}", d[0], d[1], d[2] >> 4);
    if d.len() > 4 {
        s += &format!(" {}", hex(&d[4..]));
    }
    s
}

fn hex4(d: &[u8]) -> String {
    match d {
        [h, l, ..] => format!("{h:02X}{l:02X}h"),
        _ => "?".into(),
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use knx_transport::apdu;

    #[test]
    fn confirmation_and_response() {
        let dev = Device::new(0x1101, 0x0705, 0x0083, [0, 0x83, 0, 0, 0, 1]);
        let mut bus = Bus::new(vec![dev], Log::new(None));
        let req = l_data_req(tpci::T_DATA_INDIVIDUAL, 0x11FF, 0x1101, &apdu::device_descriptor_read(0));
        let out = bus.send(&req, 0x11FF);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].message_code, message_code::L_DATA_CON);
        assert!(!LData::parse(&out[0]).unwrap().is_negative_confirm());
        let ind = LData::parse(&out[1]).unwrap();
        assert_eq!((ind.message_code, ind.src, ind.dst), (message_code::L_DATA_IND, 0x1101, 0x11FF));
        assert_eq!(ind.apdu().unwrap(), vec![0x03, 0x40, 0x07, 0x05]);

        // Nobody at 1.1.9: negative confirmation, nothing else.
        let req = l_data_req(tpci::T_CONNECT, 0x11FF, 0x1109, &[]);
        let out = bus.send(&req, 0x11FF);
        assert_eq!(out.len(), 1);
        assert!(LData::parse(&out[0]).unwrap().is_negative_confirm());
    }

    #[test]
    fn describe_frames() {
        let ld = LData::parse(&l_data_req(tpci::T_DATA_CONNECTED | tpci::seq(2), 0x11FF, 0x1101, &apdu::memory_read(0x4000, 4))).unwrap();
        assert_eq!(describe(&ld), "1.1.255 > 1.1.1 T_Data_Connected 2 A_Memory_Read 4 @4000h");
        let ld = LData::parse(&l_data_req(0, 0x11FF, 0x1101, &apdu::property_value_read(0, 11, 1, 1))).unwrap();
        assert_eq!(describe(&ld), "1.1.255 > 1.1.1 T_Data_Individual A_PropertyValue_Read obj 0 pid 11 n 1 start 1");
    }
}
