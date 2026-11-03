//! Device management over a cEMI [`Transport`]: the data link confirmation
//! and the connection-oriented Transport Layer (client side), and the
//! management services built on it.
//!
//! * Every `L_Data.req` is answered by the cEMI server with an `L_Data.con`;
//!   a set Confirm flag means the frame was not acknowledged on the bus
//!   (`03_06_03 §4.1.5.3.4`).
//! * The Transport Layer follows the Style 1 state machine of
//!   `03_03_04 §5`: `T_Connect`, numbered `T_Data_Connected` PDUs each
//!   acknowledged with `T_ACK`, repetition after 3 s or a `T_NAK` (at most
//!   3 times), 6 s connection timeout, `T_Disconnect`.
//! * `03_05_02 §3 Device Management Procedures` — `DMP_Connect` and the
//!   device descriptor / memory reads.
//! * `03_05_02 §2 Network Management Procedures` — programming the
//!   Individual Address in programming mode (`NM_IndividualAddress_Write`)
//!   or by KNX Serial Number (`NM_IndividualAddress_SerialNumber_Write`),
//!   over connectionless broadcasts.

use super::apdu::{self, apci};
use super::cemi::{self, tpci, LData};
use super::{message_code, CemiFrame, Transport};
use anyhow::{bail, Context, Result};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Acknowledgement timeout (`03_03_04 §4`).
const ACK_TIMEOUT: Duration = Duration::from_secs(3);
/// Connection timeout: the longest wait for a response (`03_03_04 §4`).
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(6);
/// Maximum number of repetitions (`03_03_04 §4`).
const MAX_REP_COUNT: usize = 3;
/// Wait for the local `L_Data.con`. The USB tunnelling timeout is 1 s
/// (`09_03 §3.4.2`); a busy bus with link layer repetitions takes longer.
const CON_TIMEOUT: Duration = Duration::from_secs(3);
/// Data octets per `A_Memory_Read` in a standard frame (`03_03_07 §3.5.3`).
const MEMORY_READ_CHUNK: usize = 12;
/// Response window of `A_IndividualAddress_Read` in
/// `NM_IndividualAddress_Write` (`03_05_02 §2.3`, step 2).
const PROG_MODE_POLL: Duration = Duration::from_secs(1);
/// Response window of `A_IndividualAddressSerialNumber_Read`; the procedure
/// gives none (`03_05_02 §2.4`), this is the `NM_IndividualAddress_Read` one.
const SERIAL_READ_WINDOW: Duration = Duration::from_secs(3);

/// A Transport Layer connection to one device (`03_03_04 §5`).
///
/// The connection is released with [`Connection::close`], or on drop.
pub struct Connection<'a> {
    t: &'a mut dyn Transport,
    dst: u16,
    seq_send: u8,
    seq_rcv: u8,
    /// `L_Data.ind` frames from the partner not consumed yet.
    inbox: VecDeque<LData>,
    open: bool,
    /// The partner sent `T_Disconnect`.
    remote_closed: bool,
}

impl<'a> Connection<'a> {
    /// `T_Connect` to the Individual Address `dst` (`DMP_Connect_RCo`,
    /// `03_05_02 §3.1`).
    pub fn open(t: &'a mut dyn Transport, dst: u16) -> Result<Self> {
        let mut c = Self::new(t, dst);
        c.send_confirmed(&cemi::l_data_control(tpci::T_CONNECT, 0, dst)).context("T_Connect")?;
        c.open = true;
        Ok(c)
    }

    /// `T_Connect`; `None` if no device acknowledged it on the bus (negative
    /// `A_Connect.Lcon`, `03_05_02 §2.3` step 1).
    pub fn open_if_present(t: &'a mut dyn Transport, dst: u16) -> Result<Option<Self>> {
        let mut c = Self::new(t, dst);
        if !c.send_confirmed_status(&cemi::l_data_control(tpci::T_CONNECT, 0, dst)).context("T_Connect")? {
            return Ok(None);
        }
        c.open = true;
        Ok(Some(c))
    }

    fn new(t: &'a mut dyn Transport, dst: u16) -> Self {
        Self { t, dst, seq_send: 0, seq_rcv: 0, inbox: VecDeque::new(), open: false, remote_closed: false }
    }

    /// The partner closed the connection with `T_Disconnect`.
    pub fn remote_closed(&self) -> bool {
        self.remote_closed
    }

    /// `A_Restart`, then abort the connection on the client side without a
    /// `T_Disconnect`, as `NM_IndividualAddress_Write` step 4 does: the
    /// device restarts and need not acknowledge.
    pub fn restart_and_abort(mut self) -> Result<()> {
        self.ensure_open()?;
        let frame = cemi::l_data_req(tpci::T_DATA_CONNECTED | tpci::seq(self.seq_send), 0, self.dst, &apdu::restart());
        self.open = false;
        self.send_confirmed(&frame)
    }

    /// Send an APDU as `T_Data_Connected` and wait for its `T_ACK`.
    pub fn send(&mut self, apdu: &[u8]) -> Result<()> {
        self.ensure_open()?;
        let frame = cemi::l_data_req(tpci::T_DATA_CONNECTED | tpci::seq(self.seq_send), 0, self.dst, apdu);
        // Data PDUs from the partner that overtake the T_ACK are kept for
        // `receive`.
        let mut early = Vec::new();
        let result = (|| {
            for _ in 0..=MAX_REP_COUNT {
                self.send_confirmed(&frame)?;
                let deadline = Instant::now() + ACK_TIMEOUT;
                while let Some(ld) = self.next_ind(deadline)? {
                    match ld.tpci() & 0xC3 {
                        tpci::T_ACK if ld.seq() == self.seq_send => {
                            self.seq_send = (self.seq_send + 1) & 0x0F;
                            return Ok(());
                        }
                        tpci::T_NAK if ld.seq() == self.seq_send => break,
                        tpci::T_ACK | tpci::T_NAK => {
                            self.disconnect();
                            bail!("{} with sequence number {} (expected {})", tpdu_name(&ld), ld.seq(), self.seq_send);
                        }
                        _ => {
                            self.check_disconnect(&ld)?;
                            early.push(ld);
                        }
                    }
                }
            }
            self.disconnect();
            bail!("no T_ACK from {} after {} repetitions", ia(self.dst), MAX_REP_COUNT)
        })();
        for ld in early.into_iter().rev() {
            self.inbox.push_front(ld);
        }
        result
    }

    /// Wait for the next `T_Data_Connected` from the partner, acknowledge
    /// it and return its APDU.
    pub fn receive(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        self.ensure_open()?;
        let deadline = Instant::now() + timeout;
        loop {
            let Some(ld) = self.next_ind(deadline)? else {
                bail!("no response from {} within {timeout:?}", ia(self.dst));
            };
            self.check_disconnect(&ld)?;
            if ld.tpci() & 0xC0 != tpci::T_DATA_CONNECTED {
                continue;
            }
            let seq = ld.seq();
            if seq == self.seq_rcv {
                self.send_control(tpci::T_ACK | tpci::seq(seq))?;
                self.seq_rcv = (self.seq_rcv + 1) & 0x0F;
                return Ok(ld.apdu().expect("data PDU"));
            } else if seq == (self.seq_rcv.wrapping_sub(1) & 0x0F) {
                // Repetition of the last PDU: our T_ACK got lost.
                self.send_control(tpci::T_ACK | tpci::seq(seq))?;
            } else {
                self.send_control(tpci::T_NAK | tpci::seq(seq))?;
            }
        }
    }

    /// Send `apdu` and return the first response APDU of the 4-bit APCI
    /// service `response`; other services are skipped.
    pub fn request(&mut self, apdu: &[u8], response: u16) -> Result<Vec<u8>> {
        self.send(apdu)?;
        let deadline = Instant::now() + CONNECTION_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let resp = self.receive(left)?;
            if apdu::is_service4(&resp, response) {
                return Ok(resp);
            }
        }
    }

    /// `T_Disconnect`.
    pub fn close(mut self) -> Result<()> {
        if self.open {
            self.open = false;
            self.send_confirmed(&cemi::l_data_control(tpci::T_DISCONNECT, 0, self.dst))?;
        }
        Ok(())
    }

    fn ensure_open(&self) -> Result<()> {
        if !self.open {
            bail!("the connection to {} is closed", ia(self.dst));
        }
        Ok(())
    }

    /// A `T_Disconnect` from the partner closes the connection.
    fn check_disconnect(&mut self, ld: &LData) -> Result<()> {
        if ld.tpci() == tpci::T_DISCONNECT {
            self.open = false;
            self.remote_closed = true;
            bail!("{} closed the connection", ia(self.dst));
        }
        Ok(())
    }

    fn disconnect(&mut self) {
        if self.open {
            self.open = false;
            let _ = self.send_confirmed(&cemi::l_data_control(tpci::T_DISCONNECT, 0, self.dst));
        }
    }

    fn send_control(&mut self, tpci: u8) -> Result<()> {
        self.send_confirmed(&cemi::l_data_control(tpci, 0, self.dst))
    }

    /// Send an `L_Data.req` and wait for a positive `L_Data.con`.
    fn send_confirmed(&mut self, frame: &CemiFrame) -> Result<()> {
        if !self.send_confirmed_status(frame)? {
            bail!("{} did not acknowledge the frame on the bus (negative L_Data.con)", ia(self.dst));
        }
        Ok(())
    }

    /// Send an `L_Data.req`; `false` for a negative `L_Data.con`. The
    /// partner's indications that arrive meanwhile are kept.
    fn send_confirmed_status(&mut self, frame: &CemiFrame) -> Result<bool> {
        let (dst, inbox) = (self.dst, &mut self.inbox);
        confirm(&mut *self.t, frame, &mut |ld| {
            if ld.src == dst && !ld.is_group() {
                inbox.push_back(ld);
            }
        })
    }

    fn keep(&mut self, ld: LData) {
        if ld.src == self.dst && !ld.is_group() {
            self.inbox.push_back(ld);
        }
    }

    /// The next `L_Data.ind` from the partner; `None` at the deadline.
    fn next_ind(&mut self, deadline: Instant) -> Result<Option<LData>> {
        loop {
            if let Some(ld) = self.inbox.pop_front() {
                return Ok(Some(ld));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            // A receive timeout is the end of the wait, not an error.
            let Ok(f) = self.t.receive(left) else { return Ok(None) };
            if f.message_code == message_code::L_DATA_IND {
                if let Ok(ld) = LData::parse(&f) {
                    self.keep(ld);
                }
            }
        }
    }
}

impl Drop for Connection<'_> {
    fn drop(&mut self) {
        self.disconnect();
    }
}

/// `A_DeviceDescriptor_Read` type 0 (`03_05_02 §3.2 DMP_Identify_R`):
/// the mask version.
pub fn device_descriptor_read(c: &mut Connection) -> Result<u16> {
    let resp = c.request(&apdu::device_descriptor_read(0), apci::DEVICE_DESCRIPTOR_RESPONSE)?;
    match apdu::parse_device_descriptor_response(&resp)? {
        (0, d) if d.len() == 2 => Ok(u16::from_be_bytes([d[0], d[1]])),
        (t, d) => bail!("unexpected device descriptor type {t} with {} octets", d.len()),
    }
}

/// Read `len` octets from `address` with `A_Memory_Read` in chunks of 12
/// (`03_05_02 §3.13 DMP_MemRead_RCo`).
pub fn memory_read(c: &mut Connection, address: u16, len: usize) -> Result<Vec<u8>> {
    if address as usize + len > 0x1_0000 {
        bail!("memory read {address:04X}h+{len} exceeds the 16-bit address space");
    }
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let at = address + out.len() as u16;
        let n = (len - out.len()).min(MEMORY_READ_CHUNK);
        let resp = c.request(&apdu::memory_read(at, n as u8), apci::MEMORY_RESPONSE)?;
        let (got_at, data) = apdu::parse_memory_response(&resp)?;
        if got_at != at || data.len() != n {
            bail!("A_Memory_Response for {got_at:04X}h ({} octets), expected {at:04X}h ({n} octets)", data.len());
        }
        out.extend_from_slice(&data);
    }
    Ok(out)
}

/// Send an `L_Data.req` and wait for its `L_Data.con`; `false` if it is
/// negative. `L_Data.ind` frames that arrive meanwhile go to `keep`.
fn confirm(t: &mut dyn Transport, frame: &CemiFrame, keep: &mut dyn FnMut(LData)) -> Result<bool> {
    let sent = LData::parse(frame)?;
    t.send(frame)?;
    let deadline = Instant::now() + CON_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            bail!("no L_Data.con from the interface within {CON_TIMEOUT:?}");
        }
        let f = t.receive(left).context("waiting for L_Data.con")?;
        let Ok(ld) = LData::parse(&f) else { continue };
        match f.message_code {
            message_code::L_DATA_CON if ld.dst == sent.dst && ld.is_group() == sent.is_group() => {
                return Ok(!ld.is_negative_confirm());
            }
            message_code::L_DATA_IND => keep(ld),
            _ => {}
        }
    }
}

/// Is `ld` a `T_Data_Broadcast` (destination 0/0/0, group)?
fn is_broadcast(ld: &LData) -> bool {
    ld.is_group() && ld.dst == 0 && ld.tpdu[0] & 0xFC == tpci::T_DATA_GROUP
}

/// Send a broadcast APDU and wait for its positive `L_Data.con`.
fn broadcast(t: &mut dyn Transport, apdu: &[u8], keep: &mut dyn FnMut(LData)) -> Result<()> {
    if !confirm(t, &cemi::l_data_broadcast(0, apdu), keep)? {
        bail!("the broadcast was not acknowledged on the bus (negative L_Data.con)");
    }
    Ok(())
}

/// Send a broadcast APDU and collect the broadcast responses with the
/// 10-bit APCI `response` during `window`: (source address, APDU).
fn broadcast_collect(t: &mut dyn Transport, apdu: &[u8], response: u16, window: Duration) -> Result<Vec<(u16, Vec<u8>)>> {
    let mut found = Vec::new();
    let mut take = |ld: LData| {
        if is_broadcast(&ld) {
            if let Some(a) = ld.apdu().filter(|a| apdu::apci_of(a) == Some(response)) {
                found.push((ld.src, a));
            }
        }
    };
    broadcast(t, apdu, &mut take)?;
    let deadline = Instant::now() + window;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        // A receive timeout ends the window.
        let Ok(f) = t.receive(left) else { break };
        if f.message_code == message_code::L_DATA_IND {
            if let Ok(ld) = LData::parse(&f) {
                take(ld);
            }
        }
    }
    Ok(found)
}

/// `NM_IndividualAddress_Read` (`03_05_02 §2.2`): the addresses of the
/// devices in programming mode, one entry per response. Responses are not
/// merged by address: two devices with the same address (e.g. two factory
/// fresh 15.15.255) answer twice. A Layer-2 repetition looks the same; for
/// `individual_address_write` that only costs one more poll.
pub fn individual_address_read(t: &mut dyn Transport, window: Duration) -> Result<Vec<u16>> {
    let resp = broadcast_collect(t, &apdu::individual_address_read(), apci::INDIVIDUAL_ADDRESS_RESPONSE, window)?;
    Ok(resp.into_iter().map(|(src, _)| src).collect())
}

/// `NM_IndividualAddress_SerialNumber_Read` (`03_05_02 §2.4`): the current
/// address of the device with `serial`, `None` if none answers.
pub fn serial_number_read(t: &mut dyn Transport, serial: &[u8; 6]) -> Result<Option<u16>> {
    let resp = broadcast_collect(t, &apdu::serial_number_read(serial), apci::SERIAL_NUMBER_RESPONSE, SERIAL_READ_WINDOW)?;
    Ok(resp
        .into_iter()
        .find(|(_, a)| apdu::parse_serial_number_response(a).is_ok_and(|sn| &sn == serial))
        .map(|(src, _)| src))
}

/// Is `addr` occupied on the network (`03_05_02 §2.3` step 1)? A device that
/// acknowledges `T_Connect` and then answers `A_DeviceDescriptor_Read` or
/// closes the connection occupies it; a negative `L_Data.con` or no
/// response means it is free.
pub fn address_in_use(t: &mut dyn Transport, addr: u16) -> Result<bool> {
    let Some(mut c) = Connection::open_if_present(t, addr)? else {
        return Ok(false);
    };
    let occupied = device_descriptor_read(&mut c).is_ok() || c.remote_closed();
    if c.open {
        let _ = c.close();
    }
    Ok(occupied)
}

/// Outcome of an Individual Address assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressAssigned {
    /// The device's address before the assignment.
    pub previous: u16,
    /// Mask version read back from the new address (programming mode only).
    pub mask: Option<u16>,
}

/// `NM_IndividualAddress_Write` (`03_05_02 §2.3`): give `new` to the one
/// device in programming mode. Waits up to `wait` for exactly one device to
/// be in programming mode, refuses an address another device uses, verifies
/// with `A_DeviceDescriptor_Read` and ends programming mode with
/// `A_Restart`. `progress` receives status messages for the user.
pub fn individual_address_write(
    t: &mut dyn Transport,
    new: u16,
    wait: Duration,
    progress: &mut dyn FnMut(&str),
) -> Result<AddressAssigned> {
    // 1. Is the new address occupied?
    let occupied = address_in_use(t, new).with_context(|| format!("checking whether {} is in use", ia(new)))?;

    // 2. Wait until exactly one device is in programming mode.
    let deadline = Instant::now() + wait;
    let mut told = None;
    let current = loop {
        let found = individual_address_read(t, PROG_MODE_POLL)?;
        if let [one] = found[..] {
            break one;
        }
        if told != Some(found.len()) {
            told = Some(found.len());
            match found.len() {
                0 => progress("waiting for a device in programming mode (press its programming button)"),
                n => progress(&format!(
                    "{n} devices in programming mode ({}); waiting until only one is",
                    found.iter().map(|&a| ia(a)).collect::<Vec<_>>().join(", ")
                )),
            }
        }
        if Instant::now() >= deadline {
            match found.len() {
                0 => bail!("no device in programming mode within {wait:?}"),
                n => bail!("{n} devices are in programming mode; only one may be"),
            }
        }
    };
    if occupied && current != new {
        bail!("{} is already used by another device (the device in programming mode is {})", ia(new), ia(current));
    }

    // 3. Set the address.
    if current != new {
        progress(&format!("{} -> {}", ia(current), ia(new)));
        broadcast(t, &apdu::individual_address_write(new), &mut |_| {})?;
    } else {
        progress(&format!("the device in programming mode already has {}", ia(new)));
    }

    // 4. Verify and leave programming mode.
    let mut c = Connection::open(t, new).with_context(|| format!("verifying {}", ia(new)))?;
    let mask = device_descriptor_read(&mut c)
        .with_context(|| format!("verifying {}: no device descriptor, the assignment may have failed", ia(new)))?;
    c.restart_and_abort()?;
    Ok(AddressAssigned { previous: current, mask: Some(mask) })
}

/// `NM_IndividualAddress_SerialNumber_Write` (`03_05_02 §2.5`): give `new`
/// to the device with `serial`, refusing an address another device uses,
/// and verify with `A_IndividualAddressSerialNumber_Read`. The device is not
/// restarted.
pub fn individual_address_serial_write(t: &mut dyn Transport, serial: &[u8; 6], new: u16) -> Result<AddressAssigned> {
    let Some(current) = serial_number_read(t, serial)? else {
        bail!("no device with serial number {} answers", serial_string(serial));
    };
    if current == new {
        return Ok(AddressAssigned { previous: current, mask: None });
    }
    if address_in_use(t, new).with_context(|| format!("checking whether {} is in use", ia(new)))? {
        bail!("{} is already used by another device", ia(new));
    }
    broadcast(t, &apdu::serial_number_write(serial, new), &mut |_| {})?;
    match serial_number_read(t, serial)? {
        Some(a) if a == new => Ok(AddressAssigned { previous: current, mask: None }),
        Some(a) => bail!("verification failed: the device still answers from {}", ia(a)),
        None => bail!("verification failed: the device no longer answers"),
    }
}

/// `mmmm:ssssssss`, the usual notation of a KNX Serial Number (manufacturer
/// code, then the manufacturer's part).
pub fn serial_string(sn: &[u8; 6]) -> String {
    format!("{:02X}{:02X}:{:02X}{:02X}{:02X}{:02X}", sn[0], sn[1], sn[2], sn[3], sn[4], sn[5])
}

fn tpdu_name(ld: &LData) -> &'static str {
    match ld.tpci() & 0xC3 {
        tpci::T_ACK => "T_ACK",
        tpci::T_NAK => "T_NAK",
        _ => "TPDU",
    }
}

/// `a.l.d` notation for log and error messages.
fn ia(a: u16) -> String {
    format!("{}.{}.{}", a >> 12, (a >> 8) & 0x0F, a & 0xFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEV: u16 = 0x1105; // 1.1.5
    const OWN: u16 = 0x11FF; // the interface's address

    /// A device on the scripted bus.
    struct Dev {
        ia: u16,
        sn: [u8; 6],
        prog: bool,
        /// Accepts Transport Layer connections (otherwise it answers
        /// `T_Connect` with `T_Disconnect`).
        tl: bool,
        mem: Vec<u8>,
        seq_rcv: u8,
        seq_send: u8,
        restarts: usize,
    }

    impl Dev {
        fn new(ia: u16, sn_tail: u8) -> Self {
            Self {
                ia,
                sn: [0x00, 0x83, 0, 0, 0, sn_tail],
                prog: false,
                tl: true,
                mem: (0..=255).collect(),
                seq_rcv: 0,
                seq_send: 0,
                restarts: 0,
            }
        }
    }

    /// A scripted bus: the interface confirms every request (negatively when
    /// no device has the destination address), the devices implement the
    /// server side of the Transport Layer, a device descriptor, a memory and
    /// the Individual Address broadcast services.
    struct Bus {
        rx: VecDeque<CemiFrame>,
        sent: Vec<LData>,
        devs: Vec<Dev>,
        /// Drop the T_ACK for this many data PDUs.
        lose_acks: usize,
        /// Answer every data PDU twice (as if our T_ACK got lost).
        repeat_responses: bool,
        nack_bus: bool,
    }

    impl Bus {
        fn new() -> Self {
            Self::with(vec![Dev::new(DEV, 1)])
        }

        fn with(devs: Vec<Dev>) -> Self {
            Self { rx: VecDeque::new(), sent: vec![], devs, lose_acks: 0, repeat_responses: false, nack_bus: false }
        }

        fn ind(&mut self, src: u16, tpci: u8, apdu: &[u8]) {
            let mut f = cemi::l_data_req(tpci, src, OWN, apdu);
            f.message_code = message_code::L_DATA_IND;
            self.rx.push_back(f);
        }

        fn broadcast_ind(&mut self, src: u16, apdu: &[u8]) {
            let mut f = cemi::l_data_broadcast(src, apdu);
            f.message_code = message_code::L_DATA_IND;
            self.rx.push_back(f);
        }

        fn respond(&mut self, i: usize, apdu: &[u8]) {
            let (src, t) = (self.devs[i].ia, tpci::T_DATA_CONNECTED | tpci::seq(self.devs[i].seq_send));
            self.ind(src, t, apdu);
            if self.repeat_responses {
                self.ind(src, t, apdu);
            }
            self.devs[i].seq_send = (self.devs[i].seq_send + 1) & 0x0F;
        }

        fn broadcast(&mut self, ld: &LData) {
            let a = ld.apdu().unwrap();
            let mut replies = Vec::new();
            match apdu::apci_of(&a) {
                Some(apci::INDIVIDUAL_ADDRESS_READ) => {
                    for d in self.devs.iter().filter(|d| d.prog) {
                        replies.push((d.ia, vec![0x01, 0x40]));
                    }
                }
                Some(apci::INDIVIDUAL_ADDRESS_WRITE) => {
                    for d in self.devs.iter_mut().filter(|d| d.prog) {
                        d.ia = u16::from_be_bytes([a[2], a[3]]);
                    }
                }
                Some(apci::SERIAL_NUMBER_READ) => {
                    for d in self.devs.iter().filter(|d| d.sn[..] == a[2..8]) {
                        let mut r = vec![0x03, 0xDD];
                        r.extend_from_slice(&d.sn);
                        r.extend_from_slice(&[0; 4]);
                        replies.push((d.ia, r));
                    }
                }
                Some(apci::SERIAL_NUMBER_WRITE) => {
                    for d in self.devs.iter_mut().filter(|d| d.sn[..] == a[2..8]) {
                        d.ia = u16::from_be_bytes([a[8], a[9]]);
                    }
                }
                _ => {}
            }
            for (src, r) in replies {
                self.broadcast_ind(src, &r);
            }
        }
    }

    impl Transport for Bus {
        fn send(&mut self, frame: &CemiFrame) -> Result<()> {
            let ld = LData::parse(frame)?;
            self.sent.push(ld.clone());
            let target = if ld.is_group() { None } else { self.devs.iter().position(|d| d.ia == ld.dst) };
            let acked = !self.nack_bus && (ld.is_group() || target.is_some());
            let mut con = frame.clone();
            con.message_code = message_code::L_DATA_CON;
            if !acked {
                con.body[0] |= 0x01;
            }
            self.rx.push_back(con);
            if !acked {
                return Ok(());
            }
            if ld.is_group() {
                self.broadcast(&ld);
                return Ok(());
            }
            let i = target.unwrap();
            let src = self.devs[i].ia;
            if ld.tpci() == tpci::T_CONNECT {
                self.devs[i].seq_rcv = 0;
                self.devs[i].seq_send = 0;
                if !self.devs[i].tl {
                    self.ind(src, tpci::T_DISCONNECT, &[]);
                }
                return Ok(());
            }
            if ld.tpci() & 0xC0 != tpci::T_DATA_CONNECTED {
                return Ok(());
            }
            if self.lose_acks > 0 {
                self.lose_acks -= 1;
                return Ok(());
            }
            let seq = ld.seq();
            self.ind(src, tpci::T_ACK | tpci::seq(seq), &[]);
            if seq != self.devs[i].seq_rcv {
                return Ok(()); // repetition, already answered
            }
            self.devs[i].seq_rcv = (seq + 1) & 0x0F;
            let apdu = ld.apdu().unwrap();
            if apdu::is_service4(&apdu, apci::DEVICE_DESCRIPTOR_READ) {
                self.respond(i, &[0x03, 0x40, 0x07, 0x05]);
            } else if apdu::is_service4(&apdu, apci::MEMORY_READ) {
                let n = (apdu[1] & 0x3F) as usize;
                let at = u16::from_be_bytes([apdu[2], apdu[3]]) as usize;
                let mut resp = vec![0x02, 0x40 | n as u8, apdu[2], apdu[3]];
                resp.extend_from_slice(&self.devs[i].mem[at..at + n]);
                self.respond(i, &resp);
            } else if apdu::apci_of(&apdu) == Some(apci::RESTART) {
                self.devs[i].prog = false;
                self.devs[i].restarts += 1;
            }
            Ok(())
        }

        fn receive(&mut self, timeout: Duration) -> Result<CemiFrame> {
            self.rx.pop_front().ok_or_else(|| anyhow::anyhow!("timeout {timeout:?}"))
        }
    }

    fn tpcis(bus: &Bus) -> Vec<u8> {
        bus.sent.iter().map(LData::tpci).collect()
    }

    #[test]
    fn descriptor_and_memory_read() {
        let mut bus = Bus::new();
        let mut c = Connection::open(&mut bus, DEV).unwrap();
        assert_eq!(device_descriptor_read(&mut c).unwrap(), 0x0705);
        assert_eq!(memory_read(&mut c, 0x10, 20).unwrap(), (0x10..0x24).collect::<Vec<u8>>());
        c.close().unwrap();
        // Connect, then per request: data + ACK of the response, disconnect.
        assert_eq!(
            tpcis(&bus),
            vec![0x80, 0x40, 0xC2, 0x44, 0xC6, 0x48, 0xCA, 0x81],
            "T_Connect, DD read seq 0, T_ACK 0, mem read seq 1, T_ACK 1, seq 2, T_ACK 2, T_Disconnect"
        );
    }

    #[test]
    fn lost_ack_is_repeated() {
        let mut bus = Bus::new();
        bus.lose_acks = 2;
        let mut c = Connection::open(&mut bus, DEV).unwrap();
        assert_eq!(device_descriptor_read(&mut c).unwrap(), 0x0705);
        drop(c);
        // The DD read went out three times with the same sequence number.
        assert_eq!(tpcis(&bus)[1..4], [0x40, 0x40, 0x40]);
        assert_eq!(*tpcis(&bus).last().unwrap(), 0x81, "drop disconnects");
    }

    #[test]
    fn repeated_response_is_acked_again() {
        let mut bus = Bus::new();
        bus.repeat_responses = true;
        let mut c = Connection::open(&mut bus, DEV).unwrap();
        assert_eq!(device_descriptor_read(&mut c).unwrap(), 0x0705);
        assert_eq!(memory_read(&mut c, 0, 2).unwrap(), vec![0, 1]);
        c.close().unwrap();
        let acks: Vec<u8> = tpcis(&bus).into_iter().filter(|t| t & 0xC3 == 0xC2).collect();
        // Each response is ACKed; the repetition of the first one is ACKed
        // again with its sequence number before the next request.
        assert_eq!(acks[..2], [0xC2, 0xC2]);
    }

    #[test]
    fn negative_confirmation_fails() {
        let mut bus = Bus::new();
        bus.nack_bus = true;
        let err = Connection::open(&mut bus, DEV).err().unwrap();
        assert!(format!("{err:#}").contains("negative L_Data.con"), "{err:#}");
    }

    #[test]
    fn remote_disconnect_closes() {
        let mut bus = Bus::new();
        let mut c = Connection::open(&mut bus, DEV).unwrap();
        let mut f = cemi::l_data_control(tpci::T_DISCONNECT, DEV, OWN);
        f.message_code = message_code::L_DATA_IND;
        c.inbox.push_back(LData::parse(&f).unwrap());
        assert!(device_descriptor_read(&mut c).is_err());
        assert!(c.send(&apdu::restart()).is_err(), "closed");
    }

    fn assign(bus: &mut Bus, new: u16) -> Result<AddressAssigned> {
        individual_address_write(bus, new, Duration::ZERO, &mut |_| {})
    }

    fn sent_service(bus: &Bus, code: u16) -> usize {
        bus.sent.iter().filter(|l| l.is_group() && l.apdu().and_then(|a| apdu::apci_of(&a)) == Some(code)).count()
    }

    fn fresh(sn_tail: u8) -> Dev {
        let mut d = Dev::new(0xFFFF, sn_tail);
        d.prog = true;
        d
    }

    #[test]
    fn prog_mode_assignment() {
        let mut bus = Bus::with(vec![Dev::new(DEV, 1), fresh(2)]);
        let r = assign(&mut bus, 0x1106).unwrap();
        assert_eq!(r, AddressAssigned { previous: 0xFFFF, mask: Some(0x0705) });
        let d = &bus.devs[1];
        assert_eq!((d.ia, d.prog, d.restarts), (0x1106, false, 1), "new address, restarted");
        assert_eq!(bus.devs[0].ia, DEV, "the other device keeps its address");
        // The connection is aborted after A_Restart: no T_Disconnect.
        assert_eq!(bus.sent.last().unwrap().apdu().unwrap(), apdu::restart());
    }

    #[test]
    fn prog_mode_refuses_an_address_in_use() {
        let mut bus = Bus::with(vec![Dev::new(DEV, 1), fresh(2)]);
        let err = assign(&mut bus, DEV).unwrap_err();
        assert!(format!("{err:#}").contains("already used"), "{err:#}");
        assert_eq!((bus.devs[1].ia, bus.devs[1].prog), (0xFFFF, true));
        assert_eq!(sent_service(&bus, apci::INDIVIDUAL_ADDRESS_WRITE), 0);
    }

    #[test]
    fn prog_mode_device_already_has_the_address() {
        let mut d = Dev::new(DEV, 1);
        d.prog = true;
        let mut bus = Bus::with(vec![d]);
        assert_eq!(assign(&mut bus, DEV).unwrap().previous, DEV);
        assert_eq!(sent_service(&bus, apci::INDIVIDUAL_ADDRESS_WRITE), 0);
        assert_eq!((bus.devs[0].prog, bus.devs[0].restarts), (false, 1));
    }

    #[test]
    fn prog_mode_needs_exactly_one_device() {
        let mut bus = Bus::new();
        let err = assign(&mut bus, 0x1106).unwrap_err();
        assert!(format!("{err:#}").contains("no device in programming mode"), "{err:#}");
        let mut bus = Bus::with(vec![fresh(2), fresh(3)]);
        let mut log = Vec::new();
        let err = individual_address_write(&mut bus, 0x1106, Duration::ZERO, &mut |m| log.push(m.to_string()))
            .unwrap_err();
        assert!(format!("{err:#}").contains("2 devices"), "{err:#}");
        assert_eq!(log.len(), 1, "{log:?}");
        assert_eq!(sent_service(&bus, apci::INDIVIDUAL_ADDRESS_WRITE), 0);
    }

    #[test]
    fn device_without_tl_occupies_the_address() {
        let mut old = Dev::new(0x1106, 1);
        old.tl = false;
        let mut bus = Bus::with(vec![old, fresh(2)]);
        assert!(address_in_use(&mut bus, 0x1106).unwrap());
        assert!(!address_in_use(&mut bus, 0x1107).unwrap());
        let err = assign(&mut bus, 0x1106).unwrap_err();
        assert!(format!("{err:#}").contains("already used"), "{err:#}");
    }

    #[test]
    fn serial_number_assignment() {
        let mut bus = Bus::with(vec![Dev::new(DEV, 1), Dev::new(0xFFFF, 2)]);
        let sn = bus.devs[1].sn;
        assert_eq!(serial_number_read(&mut bus, &sn).unwrap(), Some(0xFFFF));
        let r = individual_address_serial_write(&mut bus, &sn, 0x1107).unwrap();
        assert_eq!(r, AddressAssigned { previous: 0xFFFF, mask: None });
        assert_eq!((bus.devs[1].ia, bus.devs[1].restarts), (0x1107, 0), "not restarted");
        // Already assigned: nothing is written.
        let writes = sent_service(&bus, apci::SERIAL_NUMBER_WRITE);
        assert_eq!(individual_address_serial_write(&mut bus, &sn, 0x1107).unwrap().previous, 0x1107);
        assert_eq!(sent_service(&bus, apci::SERIAL_NUMBER_WRITE), writes);
        // Address of another device.
        let err = individual_address_serial_write(&mut bus, &sn, DEV).unwrap_err();
        assert!(format!("{err:#}").contains("already used"), "{err:#}");
        // Unknown serial number.
        let err = individual_address_serial_write(&mut bus, &[0, 0x83, 0, 0, 0, 9], 0x1108).unwrap_err();
        assert!(format!("{err:#}").contains("0083:00000009"), "{err:#}");
    }
}
