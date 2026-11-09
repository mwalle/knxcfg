//! KNXnet/IP tunnelling client (`03_08_04`): a [`Transport`] over one
//! tunnelling connection on the data link layer.
//!
//! * The connection is opened with CONNECT_REQUEST (`03_08_02 §5.2`) and
//!   closed with DISCONNECT_REQUEST, also on drop.
//! * Every cEMI frame travels in a TUNNELLING_REQUEST that the receiver
//!   confirms with a TUNNELLING_ACK; without one within 1 s the request is
//!   repeated once, then the connection is given up (`03_08_04 §2.6.1`).
//! * Received requests are acknowledged by the sequence rules of
//!   `03_08_04 §2.6.1` (the expected one and a repetition of the previous one
//!   get an ack; others are dropped).
//! * A CONNECTIONSTATE_REQUEST is sent every 60 s (`03_08_02 §5.4`).

use crate::knxip::{self, service, status, Hpai};
use crate::{CemiFrame, Transport};
use anyhow::{bail, Context, Result};
use std::collections::VecDeque;
use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

const CONNECT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const TUNNELLING_REQUEST_TIMEOUT: Duration = Duration::from_secs(1);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

pub struct IpTransport {
    sock: UdpSocket,
    /// Our endpoint as given in the HPAIs.
    local: Hpai,
    /// The server's control endpoint.
    control: SocketAddrV4,
    /// The server's data endpoint of this connection.
    data: SocketAddrV4,
    channel: u8,
    seq_send: u8,
    seq_rcv: u8,
    /// The Individual Address the server assigned to the connection.
    pub address: u16,
    /// Received cEMI frames not consumed yet.
    inbox: VecDeque<CemiFrame>,
    last_heartbeat: Instant,
    open: bool,
}

impl IpTransport {
    /// Open a tunnelling connection to the server's control endpoint.
    pub fn connect(server: SocketAddrV4) -> Result<Self> {
        // The local address towards the server, for the HPAIs.
        let probe = UdpSocket::bind("0.0.0.0:0")?;
        probe.connect(server)?;
        let SocketAddr::V4(probe_local) = probe.local_addr()? else { bail!("no IPv4 address towards {server}") };
        let sock = UdpSocket::bind(SocketAddrV4::new(*probe_local.ip(), 0)).context("bind UDP socket")?;
        let SocketAddr::V4(local) = sock.local_addr()? else { unreachable!("bound to IPv4") };
        let local = Hpai(local);

        sock.send_to(&knxip::connect_request_tunnel(local, local), server)?;
        let deadline = Instant::now() + CONNECT_REQUEST_TIMEOUT;
        let mut buf = [0u8; 1500];
        let (channel, data, address) = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("no CONNECT_RESPONSE from {server}");
            }
            sock.set_read_timeout(Some(left))?;
            let n = match sock.recv(&mut buf) {
                Ok(n) => n,
                Err(e) if is_timeout(&e) => continue,
                Err(e) => return Err(e.into()),
            };
            let Ok((svc, body)) = knxip::parse(&buf[..n]) else { continue };
            if svc != service::CONNECT_RESPONSE {
                continue;
            }
            match body {
                [channel, status::E_NO_ERROR, rest @ ..] => {
                    let (hpai, crd) = Hpai::parse(rest)?;
                    let [4, knxip::TUNNEL_CONNECTION, a0, a1, ..] = *crd else {
                        bail!("{server}: unexpected CRD {crd:02X?}");
                    };
                    break (*channel, hpai.target(server), u16::from_be_bytes([a0, a1]));
                }
                [_, st, ..] => bail!("{server} refused the tunnelling connection: status {st:02X}h"),
                _ => bail!("{server}: short CONNECT_RESPONSE"),
            }
        };
        Ok(IpTransport {
            sock,
            local,
            control: server,
            data,
            channel,
            seq_send: 0,
            seq_rcv: 0,
            address,
            inbox: VecDeque::new(),
            last_heartbeat: Instant::now(),
            open: true,
        })
    }

    /// Receive one datagram until `deadline` and handle it; returns `true`
    /// if `want_ack` (a sequence number of ours) was acknowledged.
    fn poll(&mut self, deadline: Instant, want_ack: Option<u8>) -> Result<bool> {
        if self.last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
            self.last_heartbeat = Instant::now();
            self.sock.send_to(&knxip::connectionstate_request(self.channel, self.local), self.control)?;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(false);
        }
        self.sock.set_read_timeout(Some(left))?;
        let mut buf = [0u8; 1500];
        let n = match self.sock.recv(&mut buf) {
            Ok(n) => n,
            Err(e) if is_timeout(&e) => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        let Ok((svc, body)) = knxip::parse(&buf[..n]) else { return Ok(false) };
        match svc {
            service::TUNNELLING_ACK => {
                let (h, _) = knxip::parse_connection_header(body)?;
                if h.channel == self.channel && Some(h.seq) == want_ack {
                    if h.status != status::E_NO_ERROR {
                        bail!("TUNNELLING_ACK with status {:02X}h", h.status);
                    }
                    return Ok(true);
                }
            }
            service::TUNNELLING_REQUEST => {
                let (h, cemi) = knxip::parse_connection_header(body)?;
                if h.channel != self.channel {
                    return Ok(false);
                }
                let ack = |s: &Self| s.sock.send_to(&knxip::data_ack(service::TUNNELLING_ACK, s.channel, h.seq, 0), s.data);
                if h.seq == self.seq_rcv {
                    ack(self)?;
                    self.seq_rcv = self.seq_rcv.wrapping_add(1);
                    self.inbox.push_back(CemiFrame::decode(cemi)?);
                } else if h.seq == self.seq_rcv.wrapping_sub(1) {
                    ack(self)?;
                }
            }
            service::DISCONNECT_REQUEST if body.first() == Some(&self.channel) => {
                self.open = false;
                let _ = self.sock.send_to(&knxip::disconnect_response(self.channel, 0), self.control);
                bail!("{} closed the tunnelling connection", self.control);
            }
            service::CONNECTIONSTATE_RESPONSE => {
                if let [ch, st, ..] = *body {
                    if ch == self.channel && st != status::E_NO_ERROR {
                        self.open = false;
                        bail!("{}: connection state {st:02X}h", self.control);
                    }
                }
            }
            _ => {}
        }
        Ok(false)
    }
}

impl Transport for IpTransport {
    fn send(&mut self, frame: &CemiFrame) -> Result<()> {
        if !self.open {
            bail!("the tunnelling connection is closed");
        }
        let req = knxip::data_request(service::TUNNELLING_REQUEST, self.channel, self.seq_send, &frame.encode());
        for _ in 0..2 {
            self.sock.send_to(&req, self.data)?;
            let deadline = Instant::now() + TUNNELLING_REQUEST_TIMEOUT;
            while Instant::now() < deadline {
                if self.poll(deadline, Some(self.seq_send))? {
                    self.seq_send = self.seq_send.wrapping_add(1);
                    return Ok(());
                }
            }
        }
        self.open = false;
        let _ = self.sock.send_to(&knxip::disconnect_request(self.channel, self.local), self.control);
        bail!("no TUNNELLING_ACK from {}", self.data)
    }

    fn receive(&mut self, timeout: Duration) -> Result<CemiFrame> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(f) = self.inbox.pop_front() {
                return Ok(f);
            }
            if Instant::now() >= deadline {
                bail!("no frame from {} within {timeout:?}", self.control);
            }
            self.poll(deadline, None)?;
        }
    }
}

impl Drop for IpTransport {
    fn drop(&mut self) {
        if !self.open {
            return;
        }
        let _ = self.sock.send_to(&knxip::disconnect_request(self.channel, self.local), self.control);
        // Wait briefly for the DISCONNECT_RESPONSE.
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut buf = [0u8; 1500];
        while Instant::now() < deadline {
            let _ = self.sock.set_read_timeout(Some(Duration::from_millis(100)));
            if let Ok(n) = self.sock.recv(&mut buf) {
                if matches!(knxip::parse(&buf[..n]), Ok((service::DISCONNECT_RESPONSE, _))) {
                    break;
                }
            }
        }
    }
}

fn is_timeout(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)
}
