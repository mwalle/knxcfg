//! KNXnet/IP tunnelling server (`03_08_04`) in front of the simulated bus,
//! with device management (`03_08_03`) for the server's own properties.
//!
//! One UDP socket on `<listen>:3671` serves as control and data endpoint;
//! search requests arrive on a second socket bound to the system setup
//! multicast address. One tunnelling connection and one device management
//! connection at a time.

use crate::bus::{hex, Bus};
use crate::device::{ia, Device};
use crate::knxip::{self, service, status, Description, Hpai};
use anyhow::{Context, Result};
use knx_transport::{message_code, CemiFrame};
use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

/// `03_08_04 §2.6.1`.
const TUNNELLING_REQUEST_TIMEOUT: Duration = Duration::from_secs(1);
/// `03_08_02 §5.4`: no heartbeat for this long ends the connection.
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(120);

/// cEMI local device management (`03_06_03 §4.1.7`).
mod mgmt {
    pub const M_PROP_READ_REQ: u8 = 0xFC;
    pub const M_PROP_READ_CON: u8 = 0xFB;
    pub const M_PROP_WRITE_REQ: u8 = 0xF6;
    pub const M_PROP_WRITE_CON: u8 = 0xF5;
    pub const M_RESET_REQ: u8 = 0xF1;
    /// cEMI error code "void DP": the property does not exist.
    pub const ERR_VOID_DP: u8 = 0x07;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Tunnel,
    DeviceManagement,
}

impl Kind {
    fn request(self) -> u16 {
        match self {
            Kind::Tunnel => service::TUNNELLING_REQUEST,
            Kind::DeviceManagement => service::DEVICE_CONFIGURATION_REQUEST,
        }
    }

    fn ack(self) -> u16 {
        match self {
            Kind::Tunnel => service::TUNNELLING_ACK,
            Kind::DeviceManagement => service::DEVICE_CONFIGURATION_ACK,
        }
    }
}

struct Connection {
    channel: u8,
    kind: Kind,
    control: SocketAddrV4,
    data: SocketAddrV4,
    /// Sequence counter expected from the client.
    rx: u8,
    /// Sequence counter of our next request.
    tx: u8,
    /// Our request waiting for its ack: frame, time sent, already repeated.
    pending: Option<(Vec<u8>, Instant, bool)>,
    /// cEMI frames waiting to be sent.
    queue: VecDeque<Vec<u8>>,
    last_heard: Instant,
}

pub enum Input {
    /// A datagram from the multicast socket.
    Multicast(Vec<u8>, SocketAddrV4),
    /// A command line typed by the user.
    Command(String),
}

pub struct Server {
    sock: UdpSocket,
    control: Hpai,
    desc: Description,
    tunnel_ia: u16,
    /// The server's own interface objects, read over device management.
    own: Device,
    conns: Vec<Connection>,
    next_channel: u8,
    pub bus: Bus,
}

impl Server {
    pub fn new(listen: SocketAddrV4, desc: Description, tunnel_ia: u16, bus: Bus) -> Result<Self> {
        let sock = UdpSocket::bind(listen).with_context(|| format!("bind {listen}"))?;
        sock.set_read_timeout(Some(Duration::from_millis(50)))?;
        let SocketAddr::V4(local) = sock.local_addr()? else { unreachable!("bound to IPv4") };
        // Only the device object is used. Mask 091Ah (KNX IP coupler)
        // in PID_DEVICE_DESCRIPTOR.
        let mut own = Device::new(desc.individual_address, 0x091A, u16::from_be_bytes([desc.serial[0], desc.serial[1]]), desc.serial);
        own.memory.clear();
        own.objects[0].properties.push(crate::device::Property::new(
            crate::device::pid::DEVICE_DESCRIPTOR,
            crate::device::pdt::GENERIC_02,
            2,
            &0x091Au16.to_be_bytes(),
        ));
        Ok(Server {
            sock,
            control: Hpai(local),
            desc,
            tunnel_ia,
            own,
            conns: Vec::new(),
            next_channel: 1,
            bus,
        })
    }

    /// Start the thread that receives search requests on the multicast
    /// address and passes them to `tx`.
    pub fn spawn_multicast(listen: Ipv4Addr, tx: Sender<Input>) -> Result<()> {
        let sock = UdpSocket::bind((knxip::SYSTEM_SETUP_MULTICAST, knxip::PORT)).context("bind multicast socket")?;
        sock.join_multicast_v4(&knxip::SYSTEM_SETUP_MULTICAST, &listen).context("join 224.0.23.12")?;
        std::thread::spawn(move || {
            let mut buf = [0u8; 1500];
            while let Ok((n, SocketAddr::V4(src))) = sock.recv_from(&mut buf) {
                if tx.send(Input::Multicast(buf[..n].to_vec(), src)).is_err() {
                    break;
                }
            }
        });
        Ok(())
    }

    /// Serve until a `quit` command arrives.
    pub fn run(&mut self, input: Receiver<Input>) -> Result<()> {
        self.bus.log.line(&format!(
            "listening on {}, server {}, tunnel {}",
            self.control.0,
            ia(self.desc.individual_address),
            ia(self.tunnel_ia)
        ));
        let mut buf = [0u8; 1500];
        loop {
            match self.sock.recv_from(&mut buf) {
                Ok((n, SocketAddr::V4(src))) => {
                    let d = buf[..n].to_vec();
                    self.datagram(&d, src);
                }
                Ok(_) => {}
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                Err(e) => return Err(e.into()),
            }
            while let Ok(i) = input.try_recv() {
                match i {
                    Input::Multicast(d, src) => self.datagram(&d, src),
                    Input::Command(c) => {
                        if !self.command(&c) {
                            return Ok(());
                        }
                    }
                }
            }
            self.tick();
        }
    }

    fn send(&mut self, frame: &[u8], to: SocketAddrV4) {
        if let Err(e) = self.sock.send_to(frame, to) {
            self.bus.log.line(&format!("ip      send to {to} failed: {e}"));
        }
    }

    fn datagram(&mut self, d: &[u8], src: SocketAddrV4) {
        let (svc, body) = match knxip::parse(d) {
            Ok(x) => x,
            Err(e) => {
                self.bus.log.line(&format!("ip      from {src}: {e}: {}", hex(d)));
                return;
            }
        };
        if let Err(e) = self.service(svc, body, src) {
            self.bus.log.line(&format!("ip      {} from {src}: {e:#}: {}", service::name(svc), hex(d)));
        }
    }

    fn service(&mut self, svc: u16, body: &[u8], src: SocketAddrV4) -> Result<()> {
        match svc {
            service::SEARCH_REQUEST => {
                let (hpai, _) = Hpai::parse(body)?;
                let to = hpai.target(src);
                self.bus.log.line(&format!("ip      SEARCH_REQUEST from {src}, answer to {to}"));
                let r = knxip::search_response(self.control, &self.desc);
                self.send(&r, to);
            }
            service::DESCRIPTION_REQUEST => {
                let (hpai, _) = Hpai::parse(body)?;
                self.bus.log.line(&format!("ip      DESCRIPTION_REQUEST from {src}"));
                let r = knxip::description_response(&self.desc);
                self.send(&r, hpai.target(src));
            }
            service::CONNECT_REQUEST => self.connect(body, src)?,
            service::CONNECTIONSTATE_REQUEST => {
                let channel = *body.first().context("empty body")?;
                let (hpai, _) = Hpai::parse(body.get(2..).context("short")?)?;
                let st = match self.conns.iter_mut().find(|c| c.channel == channel) {
                    Some(c) => {
                        c.last_heard = Instant::now();
                        status::E_NO_ERROR
                    }
                    None => status::E_CONNECTION_ID,
                };
                if st != status::E_NO_ERROR {
                    self.bus.log.line(&format!("ip      CONNECTIONSTATE_REQUEST for unknown channel {channel}"));
                }
                let r = knxip::connectionstate_response(channel, st);
                self.send(&r, hpai.target(src));
            }
            service::DISCONNECT_REQUEST => {
                let channel = *body.first().context("empty body")?;
                let (hpai, _) = Hpai::parse(body.get(2..).context("short")?)?;
                let st = match self.conns.iter().position(|c| c.channel == channel) {
                    Some(i) => {
                        self.conns.remove(i);
                        status::E_NO_ERROR
                    }
                    None => status::E_CONNECTION_ID,
                };
                self.bus.log.line(&format!("ip      DISCONNECT_REQUEST channel {channel} from {src}"));
                let r = knxip::disconnect_response(channel, st);
                self.send(&r, hpai.target(src));
            }
            service::DISCONNECT_RESPONSE | service::SEARCH_REQUEST_EXTENDED => {}
            service::TUNNELLING_REQUEST | service::DEVICE_CONFIGURATION_REQUEST => self.data_request(svc, body)?,
            service::TUNNELLING_ACK | service::DEVICE_CONFIGURATION_ACK => {
                let (h, _) = knxip::parse_connection_header(body)?;
                if let Some(c) = self.conns.iter_mut().find(|c| c.channel == h.channel) {
                    if c.pending.is_some() && h.seq == c.tx {
                        if h.status != status::E_NO_ERROR {
                            self.bus.log.line(&format!("ip      ack with status {:02X}h", h.status));
                        }
                        c.pending = None;
                        c.tx = c.tx.wrapping_add(1);
                        c.last_heard = Instant::now();
                    }
                }
            }
            _ => self.bus.log.line(&format!("ip      unhandled service {svc:04X}h from {src}")),
        }
        Ok(())
    }

    fn connect(&mut self, body: &[u8], src: SocketAddrV4) -> Result<()> {
        let (control, rest) = Hpai::parse(body)?;
        let (data, cri) = Hpai::parse(rest)?;
        let reply_to = control.target(src);
        let kind = match cri.get(1) {
            Some(&knxip::TUNNEL_CONNECTION) => Kind::Tunnel,
            Some(&knxip::DEVICE_MGMT_CONNECTION) => Kind::DeviceManagement,
            _ => {
                self.bus.log.line(&format!("ip      CONNECT_REQUEST with CRI {}, refused", hex(cri)));
                let r = knxip::connect_response(0, status::E_CONNECTION_TYPE, None);
                self.send(&r, reply_to);
                return Ok(());
            }
        };
        let refuse = if kind == Kind::Tunnel && cri.get(2) != Some(&knxip::TUNNEL_LINKLAYER) {
            Some(status::E_TUNNELLING_LAYER)
        } else if kind == Kind::Tunnel && cri.len() >= 6 && u16::from_be_bytes([cri[4], cri[5]]) != self.tunnel_ia {
            // Extended CRI asking for another address (`03_08_04 §5.4.3.2`):
            // E_NO_TUNNELLING_ADDRESS.
            Some(0x2D)
        } else if self.conns.iter().any(|c| c.kind == kind) {
            Some(status::E_NO_MORE_CONNECTIONS)
        } else {
            None
        };
        if let Some(st) = refuse {
            self.bus.log.line(&format!("ip      CONNECT_REQUEST {kind:?} CRI {} from {src}, refused {st:02X}h", hex(cri)));
            let r = knxip::connect_response(0, st, None);
            self.send(&r, reply_to);
            return Ok(());
        }
        let channel = self.next_channel;
        self.next_channel = self.next_channel.wrapping_add(1).max(1);
        let data = data.target(src);
        self.conns.push(Connection {
            channel,
            kind,
            control: reply_to,
            data,
            rx: 0,
            tx: 0,
            pending: None,
            queue: VecDeque::new(),
            last_heard: Instant::now(),
        });
        let crd = match kind {
            Kind::Tunnel => {
                let a = self.tunnel_ia.to_be_bytes();
                vec![4, knxip::TUNNEL_CONNECTION, a[0], a[1]]
            }
            Kind::DeviceManagement => vec![2, knxip::DEVICE_MGMT_CONNECTION],
        };
        self.bus.log.line(&format!(
            "ip      CONNECT_REQUEST {kind:?} CRI {} from {src}: channel {channel}, data {data}",
            hex(cri)
        ));
        let r = knxip::connect_response(channel, status::E_NO_ERROR, Some((self.control, &crd)));
        self.send(&r, reply_to);
        Ok(())
    }

    fn data_request(&mut self, svc: u16, body: &[u8]) -> Result<()> {
        let (h, cemi) = knxip::parse_connection_header(body)?;
        let Some(i) = self.conns.iter().position(|c| c.channel == h.channel && c.kind.request() == svc) else {
            self.bus.log.line(&format!("ip      {} for unknown channel {}", service::name(svc), h.channel));
            return Ok(());
        };
        let c = &mut self.conns[i];
        let (kind, data) = (c.kind, c.data);
        // Sequence handling, `03_08_04 §2.6.1`.
        if h.seq == c.rx.wrapping_sub(1) {
            let ack = knxip::data_ack(kind.ack(), h.channel, h.seq, status::E_NO_ERROR);
            self.send(&ack, data);
            return Ok(());
        }
        if h.seq != c.rx {
            self.bus.log.line(&format!("ip      seq {} while {} is expected, discarded", h.seq, c.rx));
            return Ok(());
        }
        c.rx = c.rx.wrapping_add(1);
        c.last_heard = Instant::now();
        let ack = knxip::data_ack(kind.ack(), h.channel, h.seq, status::E_NO_ERROR);
        self.send(&ack, data);

        let replies = match kind {
            Kind::Tunnel => self.tunnel_frame(CemiFrame::decode(cemi)?).iter().map(CemiFrame::encode).collect(),
            Kind::DeviceManagement => self.management_frame(cemi),
        };
        self.conns[i].queue.extend(replies);
        Ok(())
    }

    fn tunnel_frame(&mut self, mut frame: CemiFrame) -> Vec<CemiFrame> {
        match frame.message_code {
            message_code::L_DATA_REQ if frame.body.len() >= 6 => {
                // Source 0000h: the server enters the tunnel's address
                // (`03_08_04 §2.2.2`).
                if frame.body[2] == 0 && frame.body[3] == 0 {
                    frame.body[2..4].copy_from_slice(&self.tunnel_ia.to_be_bytes());
                }
                self.bus.send(&frame, self.tunnel_ia)
            }
            mc => {
                self.bus.log.line(&format!("tunnel  unhandled cEMI message code {mc:02X}h: {}", hex(&frame.encode())));
                Vec::new()
            }
        }
    }

    /// cEMI property services on the server's own interface objects; objects
    /// are addressed by type and instance (`03_08_03 §4.2.5`). These
    /// messages have no additional info field (`03_06_03 §4.1.7.3`), so
    /// `cemi` is the message code followed by the service information.
    fn management_frame(&mut self, cemi: &[u8]) -> Vec<Vec<u8>> {
        let Some((&code, b)) = cemi.split_first() else { return Vec::new() };
        match code {
            mgmt::M_PROP_READ_REQ | mgmt::M_PROP_WRITE_REQ if b.len() >= 6 => {
                let iot = u16::from_be_bytes([b[0], b[1]]);
                let (instance, pid) = (b[2], b[3]);
                let (count, start) = (b[4] >> 4, u16::from_be_bytes([b[4] & 0x0F, b[5]]));
                let write = code == mgmt::M_PROP_WRITE_REQ;
                // Only the device object (type 0, instance 1) exists.
                let prop = (iot == 0 && instance == 1)
                    .then(|| self.own.objects[0].properties.iter_mut().find(|p| p.id == pid))
                    .flatten();
                let name = if write { "M_PropWrite" } else { "M_PropRead" };
                let mut con = vec![if write { mgmt::M_PROP_WRITE_CON } else { mgmt::M_PROP_READ_CON }];
                con.extend_from_slice(&b[..6]);
                let value = match prop {
                    Some(p) if write => {
                        self.bus.log.line(&format!("mgmt    {name} obj {iot} pid {pid} n {count} start {start} {} (kept)", hex(&b[6..])));
                        if start > 0 && b.len() == 6 + count as usize * p.size {
                            for (k, chunk) in b[6..].chunks(p.size).enumerate() {
                                let idx = start as usize - 1 + k;
                                if p.elements.len() <= idx {
                                    p.elements.resize(idx + 1, vec![0; p.size]);
                                }
                                p.elements[idx] = chunk.to_vec();
                            }
                            Some(Vec::new())
                        } else {
                            None
                        }
                    }
                    Some(p) if start == 0 => Some((p.elements.len() as u16).to_be_bytes().to_vec()),
                    Some(p) if count > 0 && start as usize - 1 + count as usize <= p.elements.len() => {
                        Some(p.elements[start as usize - 1..start as usize - 1 + count as usize].concat())
                    }
                    _ => None,
                };
                match value {
                    Some(v) => {
                        if !write {
                            self.bus.log.line(&format!("mgmt    {name} obj {iot}/{instance} pid {pid} n {count} start {start}: {}", hex(&v)));
                        }
                        con.extend(v);
                    }
                    None => {
                        self.bus.log.line(&format!("mgmt    {name} obj {iot}/{instance} pid {pid} n {count} start {start}: no such property"));
                        con[5] &= 0x0F;
                        con.push(mgmt::ERR_VOID_DP);
                    }
                }
                vec![con]
            }
            mgmt::M_RESET_REQ => {
                self.bus.log.line("mgmt    M_Reset.req");
                Vec::new()
            }
            mc => {
                self.bus.log.line(&format!("mgmt    unhandled cEMI message code {mc:02X}h: {}", hex(cemi)));
                Vec::new()
            }
        }
    }

    /// Repetitions, queued frames and heartbeat timeouts.
    fn tick(&mut self) {
        let now = Instant::now();
        let mut out = Vec::new();
        let mut drop = Vec::new();
        for c in &mut self.conns {
            if now.duration_since(c.last_heard) > HEARTBEAT_TIMEOUT {
                drop.push((c.channel, c.control, "no heartbeat"));
                continue;
            }
            match &mut c.pending {
                Some((frame, sent, repeated)) if now.duration_since(*sent) > TUNNELLING_REQUEST_TIMEOUT => {
                    if *repeated {
                        drop.push((c.channel, c.control, "request not acknowledged"));
                    } else {
                        *repeated = true;
                        *sent = now;
                        out.push((frame.clone(), c.data));
                    }
                }
                Some(_) => {}
                None => {
                    if let Some(cemi) = c.queue.pop_front() {
                        let f = knxip::data_request(c.kind.request(), c.channel, c.tx, &cemi);
                        c.pending = Some((f.clone(), now, false));
                        out.push((f, c.data));
                    }
                }
            }
        }
        for (f, to) in out {
            self.send(&f, to);
        }
        for (channel, control, why) in drop {
            self.bus.log.line(&format!("ip      closing channel {channel}: {why}"));
            self.conns.retain(|c| c.channel != channel);
            let r = knxip::disconnect_request(channel, self.control);
            self.send(&r, control);
        }
    }

    /// A command typed by the user; false to stop.
    fn command(&mut self, line: &str) -> bool {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["quit"] | ["q"] => return false,
            ["prog", addr, rest @ ..] => match crate::parse_ia(addr) {
                Ok(a) => match self.bus.devices.iter_mut().find(|d| d.address == a) {
                    Some(d) => {
                        let on = match rest {
                            ["on"] => true,
                            ["off"] => false,
                            _ => !d.prog_mode,
                        };
                        d.set_prog_mode(on);
                        self.bus.log.line(&format!("cmd     {} programming mode {}", ia(a), if on { "on" } else { "off" }));
                    }
                    None => println!("no device {}", ia(a)),
                },
                Err(e) => println!("{e}"),
            },
            ["devices"] | ["d"] => {
                for d in &self.bus.devices {
                    let conn = d.connection().map(|p| format!(", connected to {}", ia(p))).unwrap_or_default();
                    println!("{} mask {:04X}h prog {}{conn}", ia(d.address), d.mask, if d.prog_mode { "on" } else { "off" });
                }
                for c in &self.conns {
                    println!("channel {} {:?} data {}", c.channel, c.kind, c.data);
                }
            }
            ["dump", rest @ ..] => {
                let text: String = self.bus.devices.iter().map(Device::dump).collect();
                match rest {
                    [path] => match std::fs::write(path, &text) {
                        Ok(()) => self.bus.log.line(&format!("cmd     dump written to {path}")),
                        Err(e) => println!("{path}: {e}"),
                    },
                    _ => print!("{text}"),
                }
            }
            [] => {}
            _ => println!("commands: prog <ia> [on|off], devices, dump [file], quit"),
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Log;
    use knx_transport::apdu;
    use knx_transport::cemi::{l_data_req, tpci, LData};

    fn recv(sock: &UdpSocket) -> (u16, Vec<u8>) {
        let mut buf = [0u8; 1500];
        let n = sock.recv(&mut buf).expect("reply");
        let (svc, body) = knxip::parse(&buf[..n]).unwrap();
        (svc, body.to_vec())
    }

    #[test]
    fn management_property_read() {
        let desc = Description { individual_address: 0x11FA, serial: [0; 6], mac: [0; 6], name: "t".into() };
        let mut server = Server::new("127.0.0.1:0".parse().unwrap(), desc, 0x11FB, Bus::new(Vec::new(), Log::new(None))).unwrap();
        // As sent by ETS 5.7: PID_DEVICE_DESCRIPTOR of the device object.
        let r = server.management_frame(&[0xFC, 0x00, 0x00, 0x01, 0x53, 0x10, 0x01]);
        assert_eq!(r, vec![vec![0xFB, 0x00, 0x00, 0x01, 0x53, 0x10, 0x01, 0x09, 0x1A]]);
        // Missing property: no elements, error code.
        let r = server.management_frame(&[0xFC, 0x00, 0x0B, 0x01, 0x34, 0x10, 0x01]);
        assert_eq!(r, vec![vec![0xFB, 0x00, 0x0B, 0x01, 0x34, 0x00, 0x01, 0x07]]);
    }

    #[test]
    fn tunnel_device_descriptor_read() {
        let desc = Description { individual_address: 0x11FA, serial: [0; 6], mac: [0; 6], name: "t".into() };
        let bus = Bus::new(vec![Device::new(0x1101, 0x0705, 0x0083, [0, 0x83, 0, 0, 0, 1])], Log::new(None));
        let mut server = Server::new("127.0.0.1:0".parse().unwrap(), desc, 0x11FB, bus).unwrap();
        let addr = server.control.0;
        let (tx, rx) = std::sync::mpsc::channel();
        let t = std::thread::spawn(move || server.run(rx));

        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        client.connect(addr).unwrap();
        // Route back HPAIs for control and data, CRI tunnel link layer.
        let mut body = [8, 1, 0, 0, 0, 0, 0, 0].repeat(2);
        body.extend_from_slice(&[4, knxip::TUNNEL_CONNECTION, knxip::TUNNEL_LINKLAYER, 0]);
        client.send(&knxip::frame(service::CONNECT_REQUEST, &body)).unwrap();
        let (svc, r) = recv(&client);
        assert_eq!((svc, r[1]), (service::CONNECT_RESPONSE, status::E_NO_ERROR));
        let channel = r[0];
        assert_eq!(&r[10..], &[4, 4, 0x11, 0xFB]);

        // A second tunnel is refused.
        client.send(&knxip::frame(service::CONNECT_REQUEST, &body)).unwrap();
        assert_eq!(recv(&client).1[1], status::E_NO_MORE_CONNECTIONS);

        let req = l_data_req(tpci::T_DATA_INDIVIDUAL, 0, 0x1101, &apdu::device_descriptor_read(0));
        client.send(&knxip::data_request(service::TUNNELLING_REQUEST, channel, 0, &req.encode())).unwrap();
        let (svc, r) = recv(&client);
        assert_eq!((svc, r), (service::TUNNELLING_ACK, vec![4, channel, 0, 0]));

        let mut frames = Vec::new();
        for seq in 0..2 {
            let (svc, r) = recv(&client);
            assert_eq!(svc, service::TUNNELLING_REQUEST);
            let (h, cemi) = knxip::parse_connection_header(&r).unwrap();
            assert_eq!(h.seq, seq);
            client.send(&knxip::data_ack(service::TUNNELLING_ACK, channel, seq, 0)).unwrap();
            frames.push(LData::parse(&CemiFrame::decode(cemi).unwrap()).unwrap());
        }
        // Confirmation with the tunnel address filled in, then the response.
        assert_eq!((frames[0].message_code, frames[0].src), (message_code::L_DATA_CON, 0x11FB));
        assert!(!frames[0].is_negative_confirm());
        assert_eq!((frames[1].message_code, frames[1].src, frames[1].dst), (message_code::L_DATA_IND, 0x1101, 0x11FB));
        assert_eq!(frames[1].apdu().unwrap(), vec![0x03, 0x40, 0x07, 0x05]);

        let mut body = vec![channel, 0];
        body.extend_from_slice(&[8, 1, 0, 0, 0, 0, 0, 0]);
        client.send(&knxip::frame(service::DISCONNECT_REQUEST, &body)).unwrap();
        assert_eq!(recv(&client), (service::DISCONNECT_RESPONSE, vec![channel, 0]));

        tx.send(Input::Command("quit".into())).unwrap();
        t.join().unwrap().unwrap();
    }
}
