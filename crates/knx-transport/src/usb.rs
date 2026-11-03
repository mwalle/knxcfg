//! KNX USB HID framing and the USB cEMI transport (`09_03 §3 KNX USB
//! Interface`).
//!
//! A KNX USB Interface Device is a HID class device with one 64-octet
//! interrupt endpoint per direction (`09_03 §3.3.2.4`). Every KNX frame is
//! carried in one or more HID reports:
//!
//! ```text
//! HID report header (3)          KNX USB Transfer Protocol Header (8, start packet only)
//! ReportID PacketInfo DataLen    Ver HdrLen BodyLen:2 ProtoID EMI-ID/Service Manuf:2   Body…
//!   01h    seq|type    ≤61       00h  08h     n        01h       03h          0000h    cEMI
//! ```
//!
//! * `09_03 §3.4.1.1 HID report frame structure` — report ID, packet info,
//!   data length, at most 61 data octets, unused octets filled with 00h.
//! * `09_03 §3.4.1.2.2 Sequence number` — 1 for the first report of a frame,
//!   incremented per report, at most 5 reports.
//! * `09_03 §3.4.1.2.3 Packet type` — start/end/partial bits.
//! * `09_03 §3.4.1.3 Data (KNX HID report body)` — the transfer protocol
//!   header (only in the start packet) and body.
//! * `09_03 §3.5.3 Level 2: bus access server` — the Device Feature
//!   services (protocol ID 0Fh) used to select cEMI and read the bus
//!   connection status.
//!
//! The OS access to the HID device is behind [`HidDevice`]; on Linux it is
//! [`crate::hidraw::Hidraw`].

use super::{message_code, CemiFrame, Transport};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// HID report ID for KNX data exchange (`09_03 §3.4.1.2.1`).
pub const REPORT_ID: u8 = 0x01;
/// HID report length incl. the report ID (`09_03 §3.4.1`).
pub const REPORT_LEN: usize = 64;
/// Data octets per HID report (`09_03 §3.4.1.2.4`).
const REPORT_DATA_MAX: usize = 61;
/// Maximum number of reports per KNX frame (`09_03 §3.4.1.2.2`).
const MAX_REPORTS: usize = 5;
/// Transfer protocol version 0 (`09_03 §3.4.1.3.1`).
const PROTOCOL_VERSION: u8 = 0x00;
/// Transfer protocol header length of version 0 (`09_03 §3.4.1.3.2`).
const HEADER_LEN: usize = 8;

/// Packet type bits (`09_03 §3.4.1.2.3`).
pub mod packet_type {
    pub const START: u8 = 0x01;
    pub const END: u8 = 0x02;
    pub const PARTIAL: u8 = 0x04;
}

/// Protocol IDs (`09_03 §3.4.1.3.4 Figure 17`).
pub mod protocol_id {
    pub const KNX_TUNNEL: u8 = 0x01;
    pub const BUS_ACCESS_SERVER_FEATURE: u8 = 0x0F;
}

/// EMI IDs for a KNX tunnel (`09_03 §3.4.1.3.4 Figure 18`); also the coding
/// of the "Active EMI type" feature (`09_03 §3.5.3.4.2`).
pub mod emi_id {
    pub const EMI1: u8 = 0x01;
    pub const EMI2: u8 = 0x02;
    pub const CEMI: u8 = 0x03;
}

/// Device Feature service identifiers (`09_03 §3.5.3.2.1 Figure 21`).
pub mod feature_service {
    pub const GET: u8 = 0x01;
    pub const RESPONSE: u8 = 0x02;
    pub const SET: u8 = 0x03;
    pub const INFO: u8 = 0x04;
}

/// Device Feature identifiers (`09_03 §3.5.3.3.1 Figure 29`).
pub mod feature {
    pub const SUPPORTED_EMI_TYPE: u8 = 0x01;
    pub const DEVICE_DESCRIPTOR_TYPE_0: u8 = 0x02;
    pub const BUS_CONNECTION_STATUS: u8 = 0x03;
    pub const MANUFACTURER_CODE: u8 = 0x04;
    pub const ACTIVE_EMI_TYPE: u8 = 0x05;
}

/// Bits of the "Supported EMI type" feature (`09_03 §3.5.3.3.2`).
pub mod supported_emi {
    pub const EMI1: u16 = 0x0001;
    pub const EMI2: u16 = 0x0002;
    pub const CEMI: u16 = 0x0004;
}

/// One KNX USB transfer: the transfer protocol header fields and the body
/// (`09_03 §3.4.1.3 Figure 16`).
///
/// `id` is the 6th header octet: the EMI ID of a KNX tunnel, or the service
/// identifier of a Device Feature frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferFrame {
    pub protocol_id: u8,
    pub id: u8,
    pub manufacturer: u16,
    pub body: Vec<u8>,
}

impl TransferFrame {
    /// A cEMI frame in a KNX tunnel.
    pub fn cemi(frame: &CemiFrame) -> Self {
        Self {
            protocol_id: protocol_id::KNX_TUNNEL,
            id: emi_id::CEMI,
            manufacturer: 0,
            body: frame.encode(),
        }
    }

    /// A Device Feature frame: the body is the feature identifier followed
    /// by the feature data (`09_03 §3.5.3.2`).
    pub fn feature(service: u8, feature: u8, data: &[u8]) -> Self {
        let mut body = Vec::with_capacity(1 + data.len());
        body.push(feature);
        body.extend_from_slice(data);
        Self {
            protocol_id: protocol_id::BUS_ACCESS_SERVER_FEATURE,
            id: service,
            manufacturer: 0,
            body,
        }
    }

    /// Split into HID reports (`09_03 §3.4.1.3`: the header is only in the
    /// start packet, the rest is cut after every 61 data octets).
    pub fn encode_reports(&self) -> Result<Vec<[u8; REPORT_LEN]>> {
        let body_len = u16::try_from(self.body.len())
            .map_err(|_| anyhow!("KNX USB transfer body too long: {} octets", self.body.len()))?;
        let mut data = Vec::with_capacity(HEADER_LEN + self.body.len());
        data.push(PROTOCOL_VERSION);
        data.push(HEADER_LEN as u8);
        data.extend_from_slice(&body_len.to_be_bytes());
        data.push(self.protocol_id);
        data.push(self.id);
        data.extend_from_slice(&self.manufacturer.to_be_bytes());
        data.extend_from_slice(&self.body);

        let chunks: Vec<&[u8]> = data.chunks(REPORT_DATA_MAX).collect();
        if chunks.len() > MAX_REPORTS {
            bail!("KNX USB transfer needs {} HID reports, at most {MAX_REPORTS} are allowed", chunks.len());
        }
        let last = chunks.len() - 1;
        Ok(chunks
            .iter()
            .enumerate()
            .map(|(i, chunk)| {
                let kind = match (i == 0, i == last) {
                    (true, true) => packet_type::START | packet_type::END,
                    (true, false) => packet_type::START | packet_type::PARTIAL,
                    (false, false) => packet_type::PARTIAL,
                    (false, true) => packet_type::PARTIAL | packet_type::END,
                };
                let mut report = [0u8; REPORT_LEN];
                report[0] = REPORT_ID;
                report[1] = ((i as u8 + 1) << 4) | kind;
                report[2] = chunk.len() as u8;
                report[3..3 + chunk.len()].copy_from_slice(chunk);
                report
            })
            .collect())
    }
}

/// Reassembles [`TransferFrame`]s from incoming HID reports, following the
/// error handling of `09_03 §3.4.1.2.2` and `§3.4.1.3.2`: a report with an
/// unexpected sequence number drops the frame in progress, a new start
/// packet discards an unfinished frame, a header length other than 8 rejects
/// the report.
#[derive(Debug, Default)]
pub struct Reassembler {
    /// Data octets collected so far (header included) and the next sequence
    /// number; `None` when no frame is in progress.
    partial: Option<(Vec<u8>, u8)>,
}

impl Reassembler {
    /// Feed one HID report (with its report ID). `Ok(None)` means the frame
    /// is not complete yet; an error means the report was ignored.
    pub fn push(&mut self, report: &[u8]) -> Result<Option<TransferFrame>> {
        if report.len() < 3 {
            bail!("HID report too short: {} octets", report.len());
        }
        if report[0] != REPORT_ID {
            bail!("HID report ID {:02X}h is not a KNX report", report[0]);
        }
        let seq = report[1] >> 4;
        let kind = report[1] & 0x0F;
        let len = report[2] as usize;
        if len > REPORT_DATA_MAX || 3 + len > report.len() {
            bail!("HID report data length {len} exceeds the report");
        }
        let data = &report[3..3 + len];

        if kind & packet_type::START != 0 {
            // A new start packet always discards an unfinished frame.
            self.partial = None;
            if seq != 1 {
                bail!("start packet with sequence number {seq}");
            }
            if data.len() < HEADER_LEN {
                bail!("start packet without a complete transfer header");
            }
            if data[1] as usize != HEADER_LEN {
                bail!("transfer header length {} (expected {HEADER_LEN})", data[1]);
            }
            self.partial = Some((data.to_vec(), 2));
        } else {
            match self.partial.as_mut() {
                Some((buf, next)) if *next == seq => {
                    buf.extend_from_slice(data);
                    *next += 1;
                }
                Some(_) => {
                    self.partial = None;
                    bail!("HID report with unexpected sequence number {seq}; frame dropped");
                }
                None => bail!("HID report {seq} without a start packet"),
            }
        }

        if kind & packet_type::END == 0 {
            if self.partial.as_ref().is_some_and(|(_, next)| *next as usize > MAX_REPORTS) {
                self.partial = None;
                bail!("KNX USB transfer longer than {MAX_REPORTS} reports");
            }
            return Ok(None);
        }

        let (buf, _) = self.partial.take().expect("frame in progress");
        let body_len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
        if buf.len() < HEADER_LEN + body_len {
            bail!(
                "KNX USB transfer truncated: body length {body_len}, {} octets received",
                buf.len() - HEADER_LEN
            );
        }
        Ok(Some(TransferFrame {
            protocol_id: buf[4],
            id: buf[5],
            manufacturer: u16::from_be_bytes([buf[6], buf[7]]),
            body: buf[HEADER_LEN..HEADER_LEN + body_len].to_vec(),
        }))
    }
}

/// Raw HID report access (one 64-octet report per call, report ID first).
pub trait HidDevice {
    fn write_report(&mut self, report: &[u8]) -> Result<()>;
    /// `Ok(None)` on timeout.
    fn read_report(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>>;
}

/// Timeout for a Device Feature Get (`09_03 §3.5.3.2.1`).
const FEATURE_TIMEOUT: Duration = Duration::from_secs(1);
/// Timeout for a local `M_PropWrite.con`: the KNX tunnelling timeout
/// (`09_03 §3.4.2`).
const LOCAL_TIMEOUT: Duration = Duration::from_secs(1);

/// cEMI Server Object (`03_07_03`, interface object type 8) and its
/// `PID_COMM_MODE` (`03_06_03 §4.2.2.4.2`).
const OT_CEMI_SERVER: u16 = 8;
const PID_COMM_MODE: u8 = 52;
/// `DPT_CommMode` Data Link Layer (`03_06_03 Table 17`).
const COMM_MODE_DATA_LINK_LAYER: u8 = 0x00;

/// cEMI over a KNX USB Interface Device (`09_03 §3.4.2 KNX tunnelling`).
pub struct UsbTransport<D: HidDevice> {
    dev: D,
    rx: Reassembler,
    /// cEMI frames that arrived while waiting for a feature response.
    pending: VecDeque<CemiFrame>,
    bus_connected: Option<bool>,
    /// Dump every HID report to stderr (`KNXCFG_TRACE` set).
    trace: bool,
}

impl<D: HidDevice> UsbTransport<D> {
    /// Take over an opened device and select cEMI as described in
    /// `09_03 §3.5.3.3.2`: read the supported EMI types, refuse the device
    /// if cEMI is not among them, and set the active EMI type to cEMI when
    /// the device supports more than one. Then put the cEMI server into
    /// Data Link Layer mode: its mode is undefined until the client sets
    /// `PID_COMM_MODE` (`03_06_03 §4.2.2.4.2`).
    pub fn new(dev: D) -> Result<Self> {
        let mut t = Self {
            dev,
            rx: Reassembler::default(),
            pending: VecDeque::new(),
            bus_connected: None,
            trace: std::env::var_os("KNXCFG_TRACE").is_some(),
        };

        let supported = t.feature_get(feature::SUPPORTED_EMI_TYPE).context("reading the supported EMI types")?;
        let supported = match supported[..] {
            [hi, lo] => u16::from_be_bytes([hi, lo]),
            _ => bail!("supported EMI type: expected 2 octets, got {}", supported.len()),
        };
        if supported & supported_emi::CEMI == 0 {
            bail!("the USB interface does not support cEMI (supported EMI types {supported:04X}h)");
        }
        if supported != supported_emi::CEMI {
            let active = t.feature_get(feature::ACTIVE_EMI_TYPE).context("reading the active EMI type")?;
            if active.first() != Some(&emi_id::CEMI) {
                t.feature_set(feature::ACTIVE_EMI_TYPE, &[emi_id::CEMI])?;
                let active = t.feature_get(feature::ACTIVE_EMI_TYPE)?;
                if active.first() != Some(&emi_id::CEMI) {
                    bail!("the USB interface did not switch to cEMI (active EMI type {active:02X?})");
                }
            }
        }
        let status = t.feature_get(feature::BUS_CONNECTION_STATUS).context("reading the bus connection status")?;
        t.bus_connected = status.first().map(|b| b & 0x01 != 0);
        t.set_comm_mode(COMM_MODE_DATA_LINK_LAYER).context("setting the cEMI communication mode")?;
        Ok(t)
    }

    /// `M_PropWrite.req` of `PID_COMM_MODE` in the cEMI Server Object
    /// (`03_06_03 §4.1.7.3.4`). A negative `M_PropWrite.con` is accepted:
    /// without the property the server only supports the Data Link Layer.
    fn set_comm_mode(&mut self, mode: u8) -> Result<()> {
        // MC, IOT:2, OI=1, PID, NoE=1 | SIx=1, data. Local management
        // messages carry no additional info length.
        let [iot_h, iot_l] = OT_CEMI_SERVER.to_be_bytes();
        let req = [message_code::M_PROP_WRITE_REQ, iot_h, iot_l, 1, PID_COMM_MODE, 0x10, 0x01, mode];
        let frame = TransferFrame {
            protocol_id: protocol_id::KNX_TUNNEL,
            id: emi_id::CEMI,
            manufacturer: 0,
            body: req.to_vec(),
        };
        self.write(&frame)?;
        let deadline = Instant::now() + LOCAL_TIMEOUT;
        loop {
            let Some(frame) = self.read_frame(deadline)? else {
                bail!("no M_PropWrite.con for PID_COMM_MODE within {LOCAL_TIMEOUT:?}");
            };
            match self.dispatch(frame) {
                Some(f)
                    if f.body.len() >= 7
                        && f.body[0] == message_code::M_PROP_WRITE_CON
                        && f.body[1..5] == req[1..5] =>
                {
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    /// The last known bus connection status (`09_03 §3.5.3.4`), updated by
    /// Device Feature Info frames.
    pub fn bus_connected(&self) -> Option<bool> {
        self.bus_connected
    }

    /// Device Feature Get → Response; returns the feature data.
    pub fn feature_get(&mut self, id: u8) -> Result<Vec<u8>> {
        self.write(&TransferFrame::feature(feature_service::GET, id, &[]))?;
        let deadline = Instant::now() + FEATURE_TIMEOUT;
        loop {
            let Some(frame) = self.read_frame(deadline)? else {
                bail!("no Device Feature Response for feature {id:02X}h within {FEATURE_TIMEOUT:?}");
            };
            match self.dispatch(frame) {
                Some(f) if f.id == feature_service::RESPONSE && f.body.first() == Some(&id) => {
                    return Ok(f.body[1..].to_vec());
                }
                _ => {}
            }
        }
    }

    /// Device Feature Set (not confirmed, `09_03 §3.5.3.2.3`).
    pub fn feature_set(&mut self, id: u8, data: &[u8]) -> Result<()> {
        self.write(&TransferFrame::feature(feature_service::SET, id, data))
    }

    fn write(&mut self, frame: &TransferFrame) -> Result<()> {
        for report in frame.encode_reports()? {
            if self.trace {
                eprintln!("usb tx {}", hex(&report[..3 + report[2] as usize]));
            }
            self.dev.write_report(&report)?;
        }
        Ok(())
    }

    /// Read reports until a frame is complete or the deadline passes.
    /// Malformed reports are ignored, as `09_03 §3.4.1.2.2` requires.
    fn read_frame(&mut self, deadline: Instant) -> Result<Option<TransferFrame>> {
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            let Some(report) = self.dev.read_report(left)? else {
                return Ok(None);
            };
            if self.trace {
                let n = report.get(2).map_or(report.len(), |&l| (3 + l as usize).min(report.len()));
                eprintln!("usb rx {}", hex(&report[..n]));
            }
            match self.rx.push(&report) {
                Ok(Some(frame)) => return Ok(Some(frame)),
                Ok(None) => {}
                Err(e) if self.trace => eprintln!("usb rx ignored: {e}"),
                Err(_) => {}
            }
        }
    }

    /// Queue tunnelled cEMI frames, track bus status infos, and hand back
    /// local management messages (`M_*`, no additional info) and any other
    /// Device Feature frame.
    fn dispatch(&mut self, frame: TransferFrame) -> Option<TransferFrame> {
        match frame.protocol_id {
            protocol_id::KNX_TUNNEL if frame.id == emi_id::CEMI && frame.body.first().is_some_and(|&mc| mc >= 0xF0) => {
                Some(frame)
            }
            protocol_id::KNX_TUNNEL if frame.id == emi_id::CEMI => {
                if let Ok(cemi) = CemiFrame::decode(&frame.body) {
                    self.pending.push_back(cemi);
                }
                None
            }
            protocol_id::BUS_ACCESS_SERVER_FEATURE => {
                if frame.id == feature_service::INFO
                    && frame.body.first() == Some(&feature::BUS_CONNECTION_STATUS)
                {
                    self.bus_connected = frame.body.get(1).map(|b| b & 0x01 != 0);
                    return None;
                }
                Some(frame)
            }
            _ => None,
        }
    }
}

impl<D: HidDevice> Transport for UsbTransport<D> {
    fn send(&mut self, frame: &CemiFrame) -> Result<()> {
        self.write(&TransferFrame::cemi(frame))
    }

    fn receive(&mut self, timeout: Duration) -> Result<CemiFrame> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(frame) = self.pending.pop_front() {
                return Ok(frame);
            }
            let Some(frame) = self.read_frame(deadline)? else {
                bail!("no cEMI frame from the USB interface within {timeout:?}");
            };
            self.dispatch(frame);
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_code;

    #[test]
    fn single_report_layout() {
        // `09_03 §3.4.2.1`: L_Data.req in cEMI, one start & end packet.
        let cemi = CemiFrame { message_code: message_code::L_DATA_REQ, additional_info: vec![], body: vec![0xAA; 10] };
        let reports = TransferFrame::cemi(&cemi).encode_reports().unwrap();
        assert_eq!(reports.len(), 1);
        let r = &reports[0];
        // Body = MC + AddIL + 10 = 12 octets; data length = 8 + 12.
        assert_eq!(&r[..14], &[0x01, 0x13, 20, 0x00, 0x08, 0x00, 12, 0x01, 0x03, 0x00, 0x00, 0x11, 0x00, 0xAA]);
        assert!(r[23..].iter().all(|&b| b == 0), "unused octets are 00h");
    }

    #[test]
    fn feature_get_layout() {
        // `09_03 §3.5.3.2.2 Figure 22` (example: Supported EMI type).
        let r = TransferFrame::feature(feature_service::GET, feature::SUPPORTED_EMI_TYPE, &[])
            .encode_reports()
            .unwrap();
        assert_eq!(&r[0][..12], &[0x01, 0x13, 0x09, 0x00, 0x08, 0x00, 0x01, 0x0F, 0x01, 0x00, 0x00, 0x01]);
    }

    #[test]
    fn multi_report_roundtrip() {
        // 160 body octets: 53 in the start packet, 61, then 46.
        let frame = TransferFrame { protocol_id: 1, id: 3, manufacturer: 0, body: (0..160).map(|i| i as u8).collect() };
        let reports = frame.encode_reports().unwrap();
        assert_eq!(reports.len(), 3);
        assert_eq!(reports.iter().map(|r| (r[1], r[2])).collect::<Vec<_>>(), [(0x15, 61), (0x24, 61), (0x36, 46)]);
        let mut rx = Reassembler::default();
        assert!(rx.push(&reports[0]).unwrap().is_none());
        assert!(rx.push(&reports[1]).unwrap().is_none());
        assert_eq!(rx.push(&reports[2]).unwrap(), Some(frame));
    }

    #[test]
    fn too_long_frame_is_rejected() {
        let frame = TransferFrame { protocol_id: 1, id: 3, manufacturer: 0, body: vec![0; 5 * 61] };
        assert!(frame.encode_reports().is_err());
    }

    #[test]
    fn reassembler_error_handling() {
        let frame = TransferFrame { protocol_id: 1, id: 3, manufacturer: 0, body: vec![7; 100] };
        let reports = frame.encode_reports().unwrap();
        let mut rx = Reassembler::default();
        // Out-of-sequence report drops the frame.
        rx.push(&reports[0]).unwrap();
        let mut bad = reports[1];
        bad[1] = 0x36;
        assert!(rx.push(&bad).is_err());
        assert!(rx.push(&reports[1]).is_err(), "nothing in progress any more");
        // A new start packet replaces an unfinished frame.
        rx.push(&reports[0]).unwrap();
        let single = TransferFrame::feature(feature_service::RESPONSE, 1, &[0, 4]);
        let r = single.encode_reports().unwrap();
        assert_eq!(rx.push(&r[0]).unwrap(), Some(single));
        // Header length other than 8 rejects the report.
        let mut hdr = r[0];
        hdr[4] = 9;
        assert!(rx.push(&hdr).is_err());
        // Wrong report ID.
        let mut id = r[0];
        id[0] = 0x10;
        assert!(rx.push(&id).is_err());
    }

    /// Scripted HID device: answers feature gets from a table and loops
    /// cEMI requests back as confirmations.
    struct MockHid {
        supported: u16,
        active: u8,
        out: VecDeque<Vec<u8>>,
        sets: Vec<TransferFrame>,
        rx: Reassembler,
        comm_mode: Option<u8>,
        comm_mode_missing: bool,
    }

    impl MockHid {
        fn new(supported: u16, active: u8) -> Self {
            Self {
                supported,
                active,
                out: VecDeque::new(),
                sets: vec![],
                rx: Reassembler::default(),
                comm_mode: None,
                comm_mode_missing: false,
            }
        }
        fn queue(&mut self, frame: TransferFrame) {
            for r in frame.encode_reports().unwrap() {
                self.out.push_back(r.to_vec());
            }
        }
    }

    impl HidDevice for MockHid {
        fn write_report(&mut self, report: &[u8]) -> Result<()> {
            let Some(f) = self.rx.push(report)? else { return Ok(()) };
            match (f.protocol_id, f.id) {
                (protocol_id::BUS_ACCESS_SERVER_FEATURE, feature_service::GET) => {
                    let data = match f.body[0] {
                        feature::SUPPORTED_EMI_TYPE => self.supported.to_be_bytes().to_vec(),
                        feature::ACTIVE_EMI_TYPE => vec![self.active],
                        feature::BUS_CONNECTION_STATUS => vec![1],
                        _ => return Ok(()),
                    };
                    self.queue(TransferFrame::feature(feature_service::RESPONSE, f.body[0], &data));
                }
                (protocol_id::BUS_ACCESS_SERVER_FEATURE, feature_service::SET) => {
                    if f.body[0] == feature::ACTIVE_EMI_TYPE {
                        self.active = f.body[1];
                    }
                    self.sets.push(f);
                }
                (protocol_id::KNX_TUNNEL, emi_id::CEMI) if f.body[0] == message_code::M_PROP_WRITE_REQ => {
                    self.comm_mode = Some(f.body[7]);
                    let mut body = f.body[..7].to_vec();
                    body[0] = message_code::M_PROP_WRITE_CON;
                    if self.comm_mode_missing {
                        body[5] &= 0x0F; // NoE = 0
                        body.push(0x07); // error: void DP
                    }
                    self.queue(TransferFrame { body, ..f });
                }
                (protocol_id::KNX_TUNNEL, emi_id::CEMI) => {
                    let mut body = f.body.clone();
                    body[0] = message_code::L_DATA_CON;
                    self.queue(TransferFrame { body, ..f });
                }
                _ => {}
            }
            Ok(())
        }

        fn read_report(&mut self, _timeout: Duration) -> Result<Option<Vec<u8>>> {
            Ok(self.out.pop_front())
        }
    }

    #[test]
    fn init_selects_cemi() {
        let t = UsbTransport::new(MockHid::new(supported_emi::EMI1 | supported_emi::CEMI, emi_id::EMI1)).unwrap();
        assert_eq!(t.dev.active, emi_id::CEMI);
        assert_eq!(t.dev.sets.len(), 1);
        assert_eq!(t.bus_connected(), Some(true));
        assert_eq!(t.dev.comm_mode, Some(COMM_MODE_DATA_LINK_LAYER));
        // Negative M_PropWrite.con: no PID_COMM_MODE, Data Link Layer only.
        let mut dev = MockHid::new(supported_emi::CEMI, 0);
        dev.comm_mode_missing = true;
        assert!(UsbTransport::new(dev).is_ok());
        // cEMI-only device: no Active EMI type set.
        let t = UsbTransport::new(MockHid::new(supported_emi::CEMI, 0)).unwrap();
        assert!(t.dev.sets.is_empty());
        // No cEMI at all.
        assert!(UsbTransport::new(MockHid::new(supported_emi::EMI2, emi_id::EMI2)).is_err());
    }

    #[test]
    fn cemi_send_receive_and_bus_info() {
        let mut t = UsbTransport::new(MockHid::new(supported_emi::CEMI, 0)).unwrap();
        // The bus status changes before the confirmation arrives.
        t.dev.queue(TransferFrame::feature(feature_service::INFO, feature::BUS_CONNECTION_STATUS, &[0]));
        let req = crate::cemi::l_data_control(crate::cemi::tpci::T_CONNECT, 0, 0x1101);
        t.send(&req).unwrap();
        let con = t.receive(Duration::from_secs(1)).unwrap();
        assert_eq!(con.message_code, message_code::L_DATA_CON);
        assert_eq!(con.body, req.body);
        assert_eq!(t.bus_connected(), Some(false));
        assert!(t.receive(Duration::from_millis(1)).is_err());
    }
}
