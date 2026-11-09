//! A simulated KNX device: the management server side of the Transport
//! Layer (`03_03_04 §5`) and the Application Layer services ETS uses to read
//! and program a device (`03_03_07 §3`).
//!
//! Requests the device does not know are answered as the spec says for a
//! missing resource where it says so (property and memory reads); others
//! get no answer and are marked in the log.

use knx_transport::apdu::{apci, apci_of};
use knx_transport::cemi::{tpci, LData};

/// Property datatypes (`03_07_03 §4`).
pub mod pdt {
    pub const CONTROL: u8 = 0x00;
    pub const UNSIGNED_CHAR: u8 = 0x02;
    pub const UNSIGNED_INT: u8 = 0x04;
    pub const GENERIC_02: u8 = 0x12;
    pub const GENERIC_05: u8 = 0x15;
    pub const GENERIC_06: u8 = 0x16;
    pub const GENERIC_08: u8 = 0x18;
    pub const GENERIC_10: u8 = 0x1A;
    pub const VERSION: u8 = 0x30;
    pub const BITSET8: u8 = 0x33;
}

/// Property identifiers (`03_07_03 §4.2`).
pub mod pid {
    pub const OBJECT_TYPE: u8 = 1;
    pub const LOAD_STATE_CONTROL: u8 = 5;
    pub const RUN_STATE_CONTROL: u8 = 6;
    pub const TABLE_REFERENCE: u8 = 7;
    pub const PROGRAM_VERSION: u8 = 13;
    /// Download stamp of objects 1–4 (`Stamp_SystemB` resources of mask
    /// 0705h in `knx_master.xml`).
    pub const STAMP: u8 = 27;
    pub const SERIAL_NUMBER: u8 = 11;
    pub const MANUFACTURER_ID: u8 = 12;
    pub const DEVICE_CONTROL: u8 = 14;
    pub const ORDER_INFO: u8 = 15;
    pub const VERSION: u8 = 25;
    pub const PROGMODE: u8 = 54;
    pub const MAX_APDULENGTH: u8 = 56;
    pub const HARDWARE_TYPE: u8 = 78;
    /// Device descriptor of KNXnet/IP devices.
    pub const DEVICE_DESCRIPTOR: u8 = 83;
}

/// APCI codes not used by the client side in `knx_transport::apdu`.
mod apci_ext {
    pub const PROPERTY_VALUE_RESPONSE: u16 = 0x3D6;
    pub const PROPERTY_DESCRIPTION_READ: u16 = 0x3D8;
    pub const PROPERTY_DESCRIPTION_RESPONSE: u16 = 0x3D9;
    pub const AUTHORIZE_REQUEST: u16 = 0x3D1;
    pub const AUTHORIZE_RESPONSE: u16 = 0x3D2;
    pub const ADC_READ: u16 = 0x180;
    pub const ADC_RESPONSE: u16 = 0x1C0;
}

#[derive(Debug, Clone)]
pub struct Property {
    pub id: u8,
    pub pdt: u8,
    pub writable: bool,
    /// Size of one element in octets.
    pub size: usize,
    pub max_elements: u16,
    pub elements: Vec<Vec<u8>>,
}

impl Property {
    pub fn new(id: u8, pdt: u8, size: usize, value: &[u8]) -> Self {
        assert_eq!(value.len() % size, 0);
        Property {
            id,
            pdt,
            writable: false,
            size,
            max_elements: (value.len() / size).max(1) as u16,
            elements: value.chunks(size).map(<[u8]>::to_vec).collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Object {
    /// Interface object type (PID_OBJECT_TYPE).
    pub kind: u16,
    pub properties: Vec<Property>,
    /// Load state machine, for objects of loadable parts.
    pub lsm: Option<Lsm>,
}

impl Object {
    pub fn new(kind: u16) -> Self {
        Object {
            kind,
            properties: vec![Property::new(pid::OBJECT_TYPE, pdt::UNSIGNED_INT, 2, &kind.to_be_bytes())],
            lsm: None,
        }
    }

    fn property(&mut self, id: u8) -> Option<&mut Property> {
        self.properties.iter_mut().find(|p| p.id == id)
    }

    fn set(&mut self, id: u8, value: &[u8]) {
        if let Some(p) = self.property(id) {
            p.elements = vec![value.to_vec()];
        }
    }
}

/// Load states (`03_05_01 Table 92`).
pub mod load_state {
    pub const UNLOADED: u8 = 0;
    pub const LOADED: u8 = 1;
    pub const LOADING: u8 = 2;
    pub const ERROR: u8 = 3;
    pub const UNLOADING: u8 = 4;
    pub const LOAD_COMPLETING: u8 = 5;

    pub fn name(s: u8) -> &'static str {
        match s {
            UNLOADED => "Unloaded",
            LOADED => "Loaded",
            LOADING => "Loading",
            ERROR => "Error",
            UNLOADING => "Unloading",
            LOAD_COMPLETING => "LoadCompleting",
            _ => "?",
        }
    }
}

/// A property based load state machine (`03_05_01 §4.23.2`).
#[derive(Debug, Clone, Default)]
pub struct Lsm {
    pub state: u8,
    /// The additional load controls (event 03h) of the current load, as
    /// written (10 octets each).
    pub controls: Vec<Vec<u8>>,
    /// Memory address that mirrors the state (memory mapped load status of
    /// mask 0705h, `GroupAddressTableLoadStatus` …).
    pub status_address: Option<u16>,
}

impl Lsm {
    /// Next state for a load event, `03_05_01 Table 94`; where the table
    /// allows several transitions, the recommended one.
    fn next(state: u8, event: u8) -> u8 {
        use load_state::*;
        match (event, state) {
            (0x01, UNLOADED | LOADED | LOADING) => LOADING,
            (0x01, _) => ERROR,
            (0x02, UNLOADED) => UNLOADED,
            (0x02, LOADED | LOADING) => LOADED,
            (0x02, _) => ERROR,
            (0x03, UNLOADED) => UNLOADED,
            (0x03, LOADING) => LOADING,
            (0x03, _) => ERROR,
            (0x04, _) => UNLOADED,
            // No operation and unknown events: no change.
            _ => state,
        }
    }
}

/// One frame the device sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Out {
    pub dst: u16,
    /// Destination is the broadcast address (group 0/0/0).
    pub broadcast: bool,
    pub tpci: u8,
    /// APDU in the `knx_transport::apdu` convention; empty for control PDUs.
    pub apdu: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
struct TlConnection {
    partner: u16,
    /// Sequence number expected in the next T_Data_Connected (SeqNoRcv).
    rx: u8,
    /// Sequence number of the next T_Data_Connected sent (SeqNoSend).
    tx: u8,
}

#[derive(Debug, Clone)]
pub struct Device {
    pub address: u16,
    pub serial: [u8; 6],
    /// Device descriptor type 0 (mask version).
    pub mask: u16,
    pub prog_mode: bool,
    pub objects: Vec<Object>,
    pub memory: Vec<u8>,
    /// Memory octets written by a client.
    written: Vec<bool>,
    /// Run state of the application: running after a restart with the
    /// application loaded.
    running: bool,
    conn: Option<TlConnection>,
}

impl Device {
    /// A device with a device object (index 0) holding the properties ETS
    /// reads to identify a device; memory is 64 KiB of zeros.
    pub fn new(address: u16, mask: u16, manufacturer: u16, serial: [u8; 6]) -> Self {
        // No PID_DEVICE_CONTROL, like the device in the download of
        // ETS5.log (2026-11-03; probably knx-core's emulation of the
        // BE-02001.02, not MDT hardware). Without it ETS checks memory
        // writes by reading them back.
        let mut device_object = Object::new(0);
        device_object.properties.extend([
            Property::new(pid::SERIAL_NUMBER, pdt::GENERIC_06, 6, &serial),
            Property::new(pid::MANUFACTURER_ID, pdt::UNSIGNED_INT, 2, &manufacturer.to_be_bytes()),
            Property::new(pid::ORDER_INFO, pdt::GENERIC_10, 10, &[0; 10]),
            Property::new(pid::VERSION, pdt::VERSION, 2, &[0, 0]),
            Property::new(pid::PROGMODE, pdt::BITSET8, 1, &[0]),
            Property::new(pid::MAX_APDULENGTH, pdt::UNSIGNED_INT, 2, &[0, 15]),
            Property::new(pid::HARDWARE_TYPE, pdt::GENERIC_06, 6, &[0; 6]),
        ]);
        Device {
            address,
            serial,
            mask,
            prog_mode: false,
            objects: vec![device_object],
            memory: vec![0; 0x10000],
            written: vec![false; 0x10000],
            running: false,
            conn: None,
        }
    }

    /// Add the interface object of a loadable part at `index` (objects 1–4
    /// of System 7: address table, association table, application, PEI
    /// program), with its load state machine.
    pub fn add_loadable(&mut self, index: usize, kind: u16, status_address: Option<u16>) {
        let mut o = Object::new(kind);
        let mut control = Property::new(pid::LOAD_STATE_CONTROL, pdt::CONTROL, 10, &[0; 10]);
        control.writable = true;
        o.properties.push(control);
        match kind {
            1 | 2 => o.properties.push(Property::new(pid::TABLE_REFERENCE, pdt::UNSIGNED_INT, 2, &[0, 0])),
            _ => {
                let mut run = Property::new(pid::RUN_STATE_CONTROL, pdt::CONTROL, 10, &[0; 10]);
                run.writable = true;
                let mut version = Property::new(pid::PROGRAM_VERSION, pdt::GENERIC_05, 5, &[0; 5]);
                version.writable = true;
                o.properties.extend([run, version]);
            }
        }
        let mut stamp = Property::new(pid::STAMP, pdt::GENERIC_08, 8, &[0; 8]);
        stamp.writable = true;
        o.properties.push(stamp);
        o.lsm = Some(Lsm { status_address, ..Default::default() });
        if self.objects.len() <= index {
            self.objects.resize(index + 1, Object::new(0xFFFF));
        }
        self.objects[index] = o;
        self.sync_load_status();
    }

    pub fn set_property(&mut self, object: usize, id: u8, value: &[u8]) {
        self.objects[object].set(id, value);
    }

    /// Mirror the load states into memory (memory mapped load status).
    fn sync_load_status(&mut self) {
        for o in &self.objects {
            if let Some(Lsm { state, status_address: Some(a), .. }) = o.lsm {
                self.memory[a as usize] = state;
            }
        }
    }

    /// A load event written to PID_LOAD_STATE_CONTROL of `object`.
    fn load_event(&mut self, object: usize, event: &[u8], notes: &mut Vec<String>) {
        let o = &mut self.objects[object];
        let Some(lsm) = o.lsm.as_mut() else { return };
        let before = lsm.state;
        lsm.state = Lsm::next(before, event[0]);
        match event[0] {
            0x01 if before != load_state::LOADING => lsm.controls.clear(),
            0x04 => lsm.controls.clear(),
            0x03 if lsm.state == load_state::LOADING => {
                lsm.controls.push(event.to_vec());
                // Effects of the segment controls (`03_05_02 §3.31.3.4`):
                // a data segment of a table sets its table reference, a task
                // segment carries the application id.
                match (event[1], o.kind) {
                    (0x00, 1 | 2) => o.set(pid::TABLE_REFERENCE, &event[2..4]),
                    (0x02, 3 | 4) => o.set(pid::PROGRAM_VERSION, &event[5..10]),
                    _ => {}
                }
            }
            _ => {}
        }
        let after = o.lsm.as_ref().map_or(before, |l| l.state);
        if after == load_state::ERROR && before != load_state::ERROR {
            notes.push(format!("object {object}: load event {:02X}h in state {}: Error", event[0], load_state::name(before)));
        }
        self.sync_load_status();
    }

    /// Device restart: load states (`03_05_01 Table 94`, last row) and the
    /// run state.
    fn restart(&mut self) {
        for o in &mut self.objects {
            if let Some(lsm) = o.lsm.as_mut() {
                if matches!(lsm.state, load_state::LOADING | load_state::UNLOADING) {
                    lsm.state = load_state::UNLOADED;
                }
            }
        }
        self.running = self.objects.iter().any(|o| o.kind == 3 && o.lsm.as_ref().is_some_and(|l| l.state == load_state::LOADED));
        self.sync_load_status();
        self.conn = None;
    }

    /// The device state as stable text, for comparing two downloads.
    pub fn dump(&self) -> String {
        let mut out = format!("device {}\nmask {:04X}\n", ia(self.address), self.mask);
        for (i, o) in self.objects.iter().enumerate() {
            if o.kind == 0xFFFF {
                continue;
            }
            out += &format!("object {i} type {}\n", o.kind);
            if let Some(lsm) = &o.lsm {
                out += &format!("  load state {} {}\n", lsm.state, load_state::name(lsm.state));
                for c in &lsm.controls {
                    out += &format!("  load control {}\n", hex(c));
                }
            }
            if o.kind == 3 {
                out += &format!("  run state {}\n", if self.running { "running" } else { "halted" });
            }
            for p in &o.properties {
                if matches!(p.id, pid::OBJECT_TYPE | pid::LOAD_STATE_CONTROL | pid::RUN_STATE_CONTROL) {
                    continue;
                }
                let v: Vec<String> = p.elements.iter().map(|e| hex(e)).collect();
                out += &format!("  pid {}: {}\n", p.id, v.join(" | "));
            }
        }
        out += "memory written\n";
        for row in (0..self.memory.len()).step_by(16) {
            if !self.written[row..row + 16].iter().any(|&w| w) {
                continue;
            }
            let cells: Vec<String> = (row..row + 16)
                .map(|a| if self.written[a] { format!("{:02X}", self.memory[a]) } else { "--".into() })
                .collect();
            out += &format!("  {row:04X}: {}\n", cells.join(" "));
        }
        out
    }

    pub fn set_prog_mode(&mut self, on: bool) {
        self.prog_mode = on;
        if let Some(p) = self.objects[0].property(pid::PROGMODE) {
            p.elements = vec![vec![on as u8]];
        }
    }

    /// Handle a frame seen on the bus; returns the frames the device sends.
    /// `notes` collects remarks for the log (unhandled services, …).
    pub fn receive(&mut self, ld: &LData, notes: &mut Vec<String>) -> Vec<Out> {
        if ld.is_group() {
            if ld.dst == 0 {
                return self.broadcast(ld);
            }
            return Vec::new();
        }
        if ld.dst != self.address {
            return Vec::new();
        }
        let src = ld.src;
        let ctrl = |t: u8| Out { dst: src, broadcast: false, tpci: t, apdu: Vec::new() };
        match ld.tpci() {
            tpci::T_CONNECT => match self.conn {
                Some(c) if c.partner != src => {
                    notes.push(format!("busy with {}, T_Connect refused", ia(c.partner)));
                    vec![ctrl(tpci::T_DISCONNECT)]
                }
                _ => {
                    self.conn = Some(TlConnection { partner: src, rx: 0, tx: 0 });
                    Vec::new()
                }
            },
            tpci::T_DISCONNECT => {
                if self.conn.is_some_and(|c| c.partner == src) {
                    self.conn = None;
                }
                Vec::new()
            }
            t if t & 0xC3 == tpci::T_ACK & 0xC3 => {
                if let Some(c) = self.conn.as_mut().filter(|c| c.partner == src) {
                    if ld.seq() == c.tx {
                        c.tx = (c.tx + 1) & 0x0F;
                    } else {
                        notes.push(format!("T_ACK seq {} while {} is expected", ld.seq(), c.tx));
                    }
                }
                Vec::new()
            }
            t if t & 0xC3 == tpci::T_NAK & 0xC3 => {
                notes.push("T_NAK received".into());
                Vec::new()
            }
            t if t & 0xC0 == tpci::T_DATA_CONNECTED => {
                let Some(c) = self.conn.filter(|c| c.partner == src) else {
                    notes.push("T_Data_Connected without connection".into());
                    return vec![ctrl(tpci::T_DISCONNECT)];
                };
                let seq = ld.seq();
                if seq == (c.rx + 15) & 0x0F {
                    // Repeated PDU: acknowledge again, do not process.
                    return vec![ctrl(tpci::T_ACK | tpci::seq(seq))];
                }
                if seq != c.rx {
                    return vec![ctrl(tpci::T_NAK | tpci::seq(seq))];
                }
                self.conn.as_mut().unwrap().rx = (c.rx + 1) & 0x0F;
                let mut out = vec![ctrl(tpci::T_ACK | tpci::seq(seq))];
                let apdu = ld.apdu().unwrap_or_default();
                if let Some(resp) = self.application(&apdu, notes) {
                    let tx = self.conn.map_or(0, |c| c.tx);
                    out.push(Out { dst: src, broadcast: false, tpci: tpci::T_DATA_CONNECTED | tpci::seq(tx), apdu: resp });
                }
                out
            }
            _ => {
                // T_Data_Individual (connectionless).
                let apdu = ld.apdu().unwrap_or_default();
                match self.application(&apdu, notes) {
                    Some(resp) => vec![Out { dst: src, broadcast: false, tpci: tpci::T_DATA_INDIVIDUAL, apdu: resp }],
                    None => Vec::new(),
                }
            }
        }
    }

    fn broadcast(&mut self, ld: &LData) -> Vec<Out> {
        let Some(apdu) = ld.apdu() else { return Vec::new() };
        let reply = |apdu: Vec<u8>| vec![Out { dst: 0, broadcast: true, tpci: tpci::T_DATA_GROUP, apdu }];
        match apci_of(&apdu) {
            Some(apci::INDIVIDUAL_ADDRESS_READ) if self.prog_mode => reply(apci2(apci::INDIVIDUAL_ADDRESS_RESPONSE)),
            Some(apci::INDIVIDUAL_ADDRESS_WRITE) if self.prog_mode && apdu.len() >= 4 => {
                self.address = u16::from_be_bytes([apdu[2], apdu[3]]);
                Vec::new()
            }
            Some(apci::SERIAL_NUMBER_READ) if apdu.get(2..8) == Some(&self.serial[..]) => {
                let mut r = apci2(apci::SERIAL_NUMBER_RESPONSE);
                r.extend_from_slice(&self.serial);
                r.extend_from_slice(&[0, 0, 0, 0]); // domain address, reserved
                reply(r)
            }
            Some(apci::SERIAL_NUMBER_WRITE) if apdu.get(2..8) == Some(&self.serial[..]) && apdu.len() >= 10 => {
                self.address = u16::from_be_bytes([apdu[8], apdu[9]]);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Application Layer: the response APDU to `apdu`, if any.
    fn application(&mut self, apdu: &[u8], notes: &mut Vec<String>) -> Option<Vec<u8>> {
        let code = apci_of(apdu)?;
        match code & 0x3C0 {
            apci::DEVICE_DESCRIPTOR_READ => {
                let dtype = apdu[1] & 0x3F;
                if dtype == 0 {
                    let mut r = apci2(apci::DEVICE_DESCRIPTOR_RESPONSE);
                    r.extend_from_slice(&self.mask.to_be_bytes());
                    return Some(r);
                }
                // Unsupported descriptor type: type 3Fh (`03_03_07 §3.4.3`).
                notes.push(format!("device descriptor type {dtype} not supported"));
                return Some(apci2(apci::DEVICE_DESCRIPTOR_RESPONSE | 0x3F));
            }
            apci::MEMORY_READ => {
                let count = (apdu[1] & 0x3F) as usize;
                let addr = u16::from_be_bytes([*apdu.get(2)?, *apdu.get(3)?]) as usize;
                let mut r = apci2(apci::MEMORY_RESPONSE);
                if addr + count <= self.memory.len() {
                    r[1] |= count as u8;
                    r.extend_from_slice(&(addr as u16).to_be_bytes());
                    r.extend_from_slice(&self.memory[addr..addr + count]);
                } else {
                    r.extend_from_slice(&(addr as u16).to_be_bytes());
                }
                return Some(r);
            }
            apci::MEMORY_WRITE => {
                let count = (apdu[1] & 0x3F) as usize;
                let addr = u16::from_be_bytes([*apdu.get(2)?, *apdu.get(3)?]) as usize;
                let data = apdu.get(4..4 + count)?;
                if addr + count <= self.memory.len() {
                    self.memory[addr..addr + count].copy_from_slice(data);
                    self.written[addr..addr + count].fill(true);
                }
                return None;
            }
            apci::RESTART if code == apci::RESTART => {
                self.restart();
                return None;
            }
            apci_ext::ADC_READ => {
                // No AD converter is simulated: read_count 0 is the error
                // answer (`03_03_07 §3.5.2`).
                notes.push(format!("A_ADC_Read channel {}: not simulated, error answer", apdu[1] & 0x3F));
                let mut r = apci2(apci_ext::ADC_RESPONSE | (apdu[1] & 0x3F) as u16);
                r.extend_from_slice(&[0, 0, 0]);
                return Some(r);
            }
            _ => {}
        }
        match code {
            apci::PROPERTY_VALUE_READ => Some(self.property_read(apdu.get(2..6)?)),
            apci::PROPERTY_VALUE_WRITE => Some(self.property_write(apdu.get(2..6)?, &apdu[6..], notes)),
            apci_ext::PROPERTY_DESCRIPTION_READ => Some(self.property_description(apdu.get(2..5)?)),
            apci_ext::AUTHORIZE_REQUEST => {
                // No access protection: every key gets level 0.
                let mut r = apci2(apci_ext::AUTHORIZE_RESPONSE);
                r.push(0);
                Some(r)
            }
            _ => {
                notes.push(format!("unhandled APCI {code:03X}h"));
                None
            }
        }
    }

    /// A_PropertyValue_Read (`03_03_07 §3.4.3.2`); `h` = object index, PID,
    /// nr_of_elem/start_index.
    fn property_read(&mut self, h: &[u8]) -> Vec<u8> {
        let (count, start) = (h[2] >> 4, u16::from_be_bytes([h[2] & 0x0F, h[3]]));
        if let Some(state) = self.control_state(h[0] as usize, h[1]) {
            // Load and run state: one octet (`03_05_01 §4.2.5`, `§4.2.6`).
            let data = match (start, count) {
                (0, _) => Some(vec![0, 1]),
                (1, 1) => Some(vec![state]),
                _ => None,
            };
            return property_response(h, count, start, data);
        }
        let data = self
            .objects
            .get_mut(h[0] as usize)
            .and_then(|o| o.property(h[1]))
            .and_then(|p| property_value(p, count, start));
        property_response(h, count, start, data)
    }

    fn property_write(&mut self, h: &[u8], data: &[u8], notes: &mut Vec<String>) -> Vec<u8> {
        let (count, start) = (h[2] >> 4, u16::from_be_bytes([h[2] & 0x0F, h[3]]));
        let object = h[0] as usize;
        if self.control_state(object, h[1]).is_some() {
            if (start, count, data.len()) != (1, 1, 10) {
                notes.push("load/run control write is not one 10-octet element".into());
                return property_response(h, count, start, None);
            }
            if h[1] == pid::LOAD_STATE_CONTROL {
                self.load_event(object, data, notes);
            } else {
                notes.push(format!("run control event {:02X}h ignored", data[0]));
            }
            let state = self.control_state(object, h[1]);
            return property_response(h, count, start, state.map(|s| vec![s]));
        }
        let Some(p) = self.objects.get_mut(h[0] as usize).and_then(|o| o.property(h[1])) else {
            notes.push("write to a missing property".into());
            return property_response(h, count, start, None);
        };
        if start == 0 || data.len() != count as usize * p.size {
            notes.push("property write refused".into());
            return property_response(h, count, start, None);
        }
        // Writes are kept even for read-only properties: the simulation
        // records what the client writes.
        if !p.writable {
            notes.push(format!("write to read-only PID {}", p.id));
        }
        for (i, chunk) in data.chunks(p.size).enumerate() {
            let idx = start as usize - 1 + i;
            if p.elements.len() <= idx {
                p.elements.resize(idx + 1, vec![0; p.size]);
            }
            p.elements[idx] = chunk.to_vec();
        }
        let value = property_value(p, count, start);
        property_response(h, count, start, value)
    }

    /// A_PropertyDescription_Read (`03_03_07 §3.4.3.4`); `h` = object
    /// index, PID, property index.
    fn property_description(&mut self, h: &[u8]) -> Vec<u8> {
        let mut r = apci2(apci_ext::PROPERTY_DESCRIPTION_RESPONSE);
        let obj = self.objects.get(h[0] as usize);
        let found = obj.and_then(|o| {
            if h[1] == 0 {
                o.properties.get(h[2] as usize).map(|p| (h[2], p))
            } else {
                o.properties.iter().enumerate().find(|(_, p)| p.id == h[1]).map(|(i, p)| (i as u8, p))
            }
        });
        match found {
            Some((index, p)) => {
                r.extend_from_slice(&[h[0], p.id, index, (p.writable as u8) << 7 | p.pdt]);
                r.extend_from_slice(&(p.max_elements & 0x0FFF).to_be_bytes());
                r.push(if p.writable { 0x33 } else { 0x30 });
            }
            None => r.extend_from_slice(&[h[0], h[1], h[2], 0, 0, 0, 0]),
        }
        r
    }

    /// The state behind PID_LOAD_STATE_CONTROL or PID_RUN_STATE_CONTROL of
    /// an object with a load state machine.
    fn control_state(&self, object: usize, id: u8) -> Option<u8> {
        let o = self.objects.get(object)?;
        let lsm = o.lsm.as_ref()?;
        match id {
            pid::LOAD_STATE_CONTROL => Some(lsm.state),
            pid::RUN_STATE_CONTROL if o.kind == 3 || o.kind == 4 => Some((o.kind == 3 && self.running) as u8),
            _ => None,
        }
    }

    /// The partner of the open Transport Layer connection.
    pub fn connection(&self) -> Option<u16> {
        self.conn.map(|c| c.partner)
    }
}

fn property_value(p: &Property, count: u8, start: u16) -> Option<Vec<u8>> {
    if start == 0 {
        // Current number of elements (`03_03_07 §3.4.3.2`, count is 1).
        return Some((p.elements.len() as u16).to_be_bytes().to_vec());
    }
    let first = start as usize - 1;
    let last = first + count as usize;
    if count == 0 || last > p.elements.len() {
        return None;
    }
    Some(p.elements[first..last].concat())
}

fn property_response(h: &[u8], count: u8, start: u16, data: Option<Vec<u8>>) -> Vec<u8> {
    let mut r = apci2(apci_ext::PROPERTY_VALUE_RESPONSE);
    r.extend_from_slice(&h[..2]);
    // Error: nr_of_elem 0, no data.
    let count = match (&data, start) {
        (None, _) => 0,
        (Some(_), 0) => 1,
        (Some(_), _) => count,
    };
    r.push(count << 4 | (start >> 8) as u8);
    r.push(start as u8);
    if let Some(d) = data {
        r.extend(d);
    }
    r
}

/// The two APCI octets of a 10-bit service code.
fn apci2(code: u16) -> Vec<u8> {
    vec![(code >> 8) as u8 & 0x03, code as u8]
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ")
}

pub fn ia(a: u16) -> String {
    format!("{}.{}.{}", a >> 12, (a >> 8) & 0x0F, a & 0xFF)
}

#[cfg(test)]
mod tests {
    use super::*;
    use knx_transport::apdu;
    use knx_transport::cemi::l_data_req;

    const ME: u16 = 0x1101;
    const ETS: u16 = 0x11FF;

    fn ld(tpci: u8, src: u16, dst: u16, apdu: &[u8]) -> LData {
        LData::parse(&l_data_req(tpci, src, dst, apdu)).unwrap()
    }

    fn dev() -> Device {
        Device::new(ME, 0x0705, 0x0083, [0x00, 0x83, 1, 2, 3, 4])
    }

    #[test]
    fn connected_device_descriptor_read() {
        let mut d = dev();
        let mut n = Vec::new();
        assert!(d.receive(&ld(tpci::T_CONNECT, ETS, ME, &[]), &mut n).is_empty());
        let out = d.receive(&ld(tpci::T_DATA_CONNECTED, ETS, ME, &apdu::device_descriptor_read(0)), &mut n);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].tpci, tpci::T_ACK);
        assert_eq!(out[1].tpci, tpci::T_DATA_CONNECTED);
        assert_eq!(out[1].apdu, vec![0x03, 0x40, 0x07, 0x05]);
        // The client acknowledges; the next response uses seq 1.
        d.receive(&ld(tpci::T_ACK, ETS, ME, &[]), &mut n);
        let out = d.receive(&ld(tpci::T_DATA_CONNECTED | tpci::seq(1), ETS, ME, &apdu::memory_read(0x60, 1)), &mut n);
        assert_eq!(out[0].tpci, tpci::T_ACK | tpci::seq(1));
        assert_eq!(out[1].tpci, tpci::T_DATA_CONNECTED | tpci::seq(1));
        assert_eq!(out[1].apdu, vec![0x02, 0x41, 0x00, 0x60, 0x00]);
        // Repetition of seq 1: ACK again, no response.
        let out = d.receive(&ld(tpci::T_DATA_CONNECTED | tpci::seq(1), ETS, ME, &apdu::memory_read(0x60, 1)), &mut n);
        assert_eq!(out.len(), 1);
        // Wrong sequence number: NAK.
        let out = d.receive(&ld(tpci::T_DATA_CONNECTED | tpci::seq(5), ETS, ME, &apdu::memory_read(0x60, 1)), &mut n);
        assert_eq!(out[0].tpci, tpci::T_NAK | tpci::seq(5));
        // Another client is refused while connected.
        let out = d.receive(&ld(tpci::T_CONNECT, 0x11FE, ME, &[]), &mut n);
        assert_eq!(out[0].tpci, tpci::T_DISCONNECT);
        assert!(n.iter().any(|s| s.contains("busy")));
    }

    #[test]
    fn properties() {
        let mut d = dev();
        let mut n = Vec::new();
        let read = |d: &mut Device, n: &mut Vec<String>, pid, count, start| {
            d.receive(&ld(tpci::T_DATA_INDIVIDUAL, ETS, ME, &apdu::property_value_read(0, pid, count, start)), n)[0].apdu.clone()
        };
        assert_eq!(read(&mut d, &mut n, pid::SERIAL_NUMBER, 1, 1), vec![0x03, 0xD6, 0, 11, 0x10, 0x01, 0x00, 0x83, 1, 2, 3, 4]);
        // Element count.
        assert_eq!(read(&mut d, &mut n, pid::SERIAL_NUMBER, 1, 0), vec![0x03, 0xD6, 0, 11, 0x10, 0x00, 0x00, 0x01]);
        // Missing property: nr_of_elem 0.
        assert_eq!(read(&mut d, &mut n, 200, 1, 1), vec![0x03, 0xD6, 0, 200, 0x00, 0x01]);
        let desc = d.receive(&ld(tpci::T_DATA_INDIVIDUAL, ETS, ME, &[0x03, 0xD8, 0, pid::MANUFACTURER_ID, 0]), &mut n);
        assert_eq!(desc[0].apdu, vec![0x03, 0xD9, 0, 12, 2, pdt::UNSIGNED_INT, 0x00, 0x01, 0x30]);
        let desc = d.receive(&ld(tpci::T_DATA_INDIVIDUAL, ETS, ME, &[0x03, 0xD8, 3, 1, 0]), &mut n);
        assert_eq!(desc[0].apdu, vec![0x03, 0xD9, 3, 1, 0, 0, 0, 0, 0]);
        // A_ADC_Read channel 1, 8 reads (ETS device info): error answer.
        let adc = d.receive(&ld(tpci::T_DATA_INDIVIDUAL, ETS, ME, &[0x01, 0x81, 0x08]), &mut n);
        assert_eq!(adc[0].apdu, vec![0x01, 0xC1, 0, 0, 0]);
    }

    #[test]
    fn load_state_machine() {
        use load_state::*;
        let mut d = dev();
        d.add_loadable(1, 1, Some(0xB6EA));
        let mut n = Vec::new();
        let mut event = |d: &mut Device, e: &[u8]| {
            let mut data = e.to_vec();
            data.resize(10, 0);
            let apdu = knx_transport::apdu::property_value_write(1, pid::LOAD_STATE_CONTROL, 1, 1, &data);
            let out = d.receive(&ld(tpci::T_DATA_INDIVIDUAL, ETS, ME, &apdu), &mut n);
            *out[0].apdu.last().unwrap()
        };
        // Segment controls outside Loading are ignored (Table 94: R).
        assert_eq!(event(&mut d, &[0x03, 0x00, 0x40, 0x00]), UNLOADED);
        assert_eq!(event(&mut d, &[0x01]), LOADING);
        assert_eq!(event(&mut d, &[0x03, 0x00, 0x40, 0x00, 0x02, 0x01, 0xFF, 0x03, 0x80]), LOADING);
        assert_eq!(d.memory[0xB6EA], LOADING);
        assert_eq!(event(&mut d, &[0x02]), LOADED);
        assert!(d.dump().contains("  pid 7: 40 00\n"));
        // An additional load control in Loaded is an error; Unload recovers.
        assert_eq!(event(&mut d, &[0x03, 0x00]), ERROR);
        assert_eq!(event(&mut d, &[0x01]), ERROR);
        assert_eq!(event(&mut d, &[0x04]), UNLOADED);
        assert!(!d.dump().contains("load control"));
        // A restart while loading leaves the part unloaded.
        event(&mut d, &[0x01]);
        d.receive(&ld(tpci::T_DATA_INDIVIDUAL, ETS, ME, &knx_transport::apdu::restart()), &mut n);
        assert_eq!(d.memory[0xB6EA], UNLOADED);
    }

    #[test]
    fn broadcast_in_programming_mode() {
        let mut d = dev();
        let mut n = Vec::new();
        let mut req = l_data_req(tpci::T_DATA_GROUP, ETS, 0, &apdu::individual_address_read());
        req.body[1] |= 0x80;
        let req = LData::parse(&req).unwrap();
        assert!(d.receive(&req, &mut n).is_empty());
        d.set_prog_mode(true);
        let out = d.receive(&req, &mut n);
        assert_eq!(out, vec![Out { dst: 0, broadcast: true, tpci: 0, apdu: vec![0x01, 0x40] }]);
    }
}
