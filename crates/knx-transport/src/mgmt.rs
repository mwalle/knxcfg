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
}

impl<'a> Connection<'a> {
    /// `T_Connect` to the Individual Address `dst` (`DMP_Connect_RCo`,
    /// `03_05_02 §3.1`).
    pub fn open(t: &'a mut dyn Transport, dst: u16) -> Result<Self> {
        let mut c = Self { t, dst, seq_send: 0, seq_rcv: 0, inbox: VecDeque::new(), open: false };
        c.send_confirmed(&cemi::l_data_control(tpci::T_CONNECT, 0, dst)).context("T_Connect")?;
        c.open = true;
        Ok(c)
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

    /// Send an `L_Data.req` and wait for its `L_Data.con`, keeping the
    /// partner's indications that arrive meanwhile.
    fn send_confirmed(&mut self, frame: &CemiFrame) -> Result<()> {
        self.t.send(frame)?;
        let deadline = Instant::now() + CON_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("no L_Data.con from the interface within {CON_TIMEOUT:?}");
            }
            let f = self.t.receive(left).context("waiting for L_Data.con")?;
            let Ok(ld) = LData::parse(&f) else { continue };
            match f.message_code {
                message_code::L_DATA_CON if ld.dst == self.dst && !ld.is_group() => {
                    if ld.is_negative_confirm() {
                        bail!("{} did not acknowledge the frame on the bus (negative L_Data.con)", ia(self.dst));
                    }
                    return Ok(());
                }
                message_code::L_DATA_IND => self.keep(ld),
                _ => {}
            }
        }
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

    /// A scripted bus: the interface confirms every request, the remote
    /// device implements the server side of the Transport Layer with a
    /// device descriptor and a memory.
    struct Bus {
        rx: VecDeque<CemiFrame>,
        sent: Vec<LData>,
        seq_rcv: u8,
        seq_send: u8,
        mem: Vec<u8>,
        /// Drop the T_ACK for this many data PDUs.
        lose_acks: usize,
        /// Answer every data PDU twice (as if our T_ACK got lost).
        repeat_responses: bool,
        nack_bus: bool,
    }

    impl Bus {
        fn new() -> Self {
            Self {
                rx: VecDeque::new(),
                sent: vec![],
                seq_rcv: 0,
                seq_send: 0,
                mem: (0..=255).collect(),
                lose_acks: 0,
                repeat_responses: false,
                nack_bus: false,
            }
        }

        fn ind(&mut self, tpci: u8, apdu: &[u8]) {
            let mut f = cemi::l_data_req(tpci, DEV, OWN, apdu);
            f.message_code = message_code::L_DATA_IND;
            self.rx.push_back(f);
        }

        fn respond(&mut self, apdu: &[u8]) {
            let t = tpci::T_DATA_CONNECTED | tpci::seq(self.seq_send);
            self.ind(t, apdu);
            if self.repeat_responses {
                self.ind(t, apdu);
            }
            self.seq_send = (self.seq_send + 1) & 0x0F;
        }
    }

    impl Transport for Bus {
        fn send(&mut self, frame: &CemiFrame) -> Result<()> {
            let ld = LData::parse(frame)?;
            let mut con = frame.clone();
            con.message_code = message_code::L_DATA_CON;
            if self.nack_bus {
                con.body[0] |= 0x01;
            }
            self.rx.push_back(con);
            self.sent.push(ld.clone());
            if self.nack_bus || ld.tpci() & 0xC0 != tpci::T_DATA_CONNECTED {
                return Ok(());
            }
            if self.lose_acks > 0 {
                self.lose_acks -= 1;
                return Ok(());
            }
            let seq = ld.seq();
            self.ind(tpci::T_ACK | tpci::seq(seq), &[]);
            if seq != self.seq_rcv {
                return Ok(()); // repetition, already answered
            }
            self.seq_rcv = (self.seq_rcv + 1) & 0x0F;
            let apdu = ld.apdu().unwrap();
            if apdu::is_service4(&apdu, apci::DEVICE_DESCRIPTOR_READ) {
                self.respond(&[0x03, 0x40, 0x07, 0x05]);
            } else if apdu::is_service4(&apdu, apci::MEMORY_READ) {
                let n = (apdu[1] & 0x3F) as usize;
                let at = u16::from_be_bytes([apdu[2], apdu[3]]) as usize;
                let mut resp = vec![0x02, 0x40 | n as u8, apdu[2], apdu[3]];
                resp.extend_from_slice(&self.mem[at..at + n]);
                self.respond(&resp);
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
}
