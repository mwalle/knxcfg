//! knx-transport's KNXnet/IP client against knx-sim's server on localhost.

use knx_sim::bus::{Bus, Log};
use knx_sim::device::Device;
use knx_sim::knxip::Description;
use knx_sim::server::{Input, Server};
use knx_transport::ip::IpTransport;
use knx_transport::mgmt::{self, Connection};

#[test]
fn device_descriptor_and_memory_over_ip() {
    let desc = Description { individual_address: 0x11FA, serial: [0; 6], mac: [0; 6], name: "t".into() };
    let mut dev = Device::new(0x1101, 0x0705, 0x0083, [0, 0x83, 0, 0, 0, 1]);
    dev.memory[0x4000..0x4004].copy_from_slice(&[1, 2, 3, 4]);
    let mut server = Server::new("127.0.0.1:0".parse().unwrap(), desc, 0x11FB, Bus::new(vec![dev], Log::to_file(None))).unwrap();
    let addr = server.local_addr();
    let (tx, rx) = std::sync::mpsc::channel();
    let t = std::thread::spawn(move || server.run(rx));

    {
        let mut ip = IpTransport::connect(addr).unwrap();
        assert_eq!(ip.address, 0x11FB);
        let mut c = Connection::open(&mut ip, 0x1101).unwrap();
        assert_eq!(mgmt::device_descriptor_read(&mut c).unwrap(), 0x0705);
        assert_eq!(mgmt::memory_read(&mut c, 0x4000, 4).unwrap(), vec![1, 2, 3, 4]);
        c.close().unwrap();
        // A device that does not exist: negative L_Data.con.
        assert!(Connection::open_if_present(&mut ip, 0x1109).unwrap().is_none());
    }

    tx.send(Input::Command("quit".into())).unwrap();
    t.join().unwrap().unwrap();
}
