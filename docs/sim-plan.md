# knx-sim: verifying downloads against ETS without real devices

Status (2026-11-10): steps 1 and 2 done, `crates/knx-sim`; tested with ETS
5.7.4. Next is step 3.

Run it with
`cargo run -p knx-sim -- --listen 192.168.77.1 --product 1.1.1:<knxprod>[:<h2p>] --log <file>`
(or `--device <ia>:<mask>[:<manufacturer>]` for a device without a knxprod).
Commands on stdin: `prog <ia> [on|off]`, `devices`, `dump [file]`, `quit`.
Without a network: `--replay <file>` sends recorded requests (`> <cEMI>`
lines) or `knxcfg download --frames` output to the devices and prints their
dumps.

## Why

The import and the download builder (M2, M5) produce download plans that have
mostly never been compared with ETS:

- the parameter image of every product (only the System 7 tables of the
  BE-02001.02 are checked, `crates/knx-prod/tests/oracle.rs`);
- the System B tables and the partial download;
- the coupler download (filter table contents and the `A_FilterTable_*`
  services, see `STATUS.md` "Couplers");
- whether the first entry of `ComObjectInstanceRef/@Links` (ETS 5.7) is the
  sending address (`knxproj-format.md`).

The live download itself (M4) is also missing. The hardware run with the USB
interface got no `L_Data.con`, so that path is blocked for now.

Downloading to the devices of the real installation is not an option for
testing. The project in `refs/knxproj/Haus.knxprojarchive` is private (keep its
contents out of tracked files).

## Idea

Emulate a KNXnet/IP tunnelling interface in software, with simulated devices
behind it. ETS (in a Windows VM) uses the emulator as its bus connection
instead of the USB interface, so it never reaches the real bus. The emulator
controls every telegram in both directions.

```
ETS (Windows VM) --KNXnet/IP tunnelling, UDP 3671--> knx-sim (Linux)
                                                       simulated bus
                                                       +- coupler, mask 091A
                                                       +- BE-02001.02, mask 0705
                                                       +- any device of a project
```

The test that matters compares **device state**, not telegrams:

1. ETS downloads a device into the simulation; dump its memory, properties and
   load states.
2. `knxcfg` downloads the same configuration; dump again.
3. Equal dumps mean `knxcfg` writes what ETS writes.

The telegram logs are kept as well: they show what ETS reads and in which
order, and they are the reference for the plan steps.

The emulator also gives `knxcfg` an IP transport, so the live download (M4)
can be built and tested against it while the USB path is blocked.

## Components (new crate `crates/knx-sim`)

1. **Tunnelling server**: `SEARCH` (multicast 224.0.23.12:3671), `DESCRIPTION`,
   `CONNECT` (tunnel, link layer), `CONNECTIONSTATE`, `DISCONNECT`,
   `TUNNELLING_REQUEST`/`ACK`; cEMI `L_Data.req` answered by `L_Data.con`,
   responses sent as `L_Data.ind`. Possibly `DEVICE_CONFIGURATION` for the
   interface's own properties (ETS reads its address). Spec: volume 3 part 8
   (KNXnet/IP), not yet in `docs/spec-index.md`.
2. **Simulated bus**: routes frames between the tunnel and the devices by
   address, gives the acknowledgements, logs every frame.
3. **Simulated device** (management server): Transport Layer connection
   (`T_Connect`, sequence numbers, `T_ACK`, timeouts), device descriptor 0,
   memory, interface objects and properties, load state machines,
   `A_Restart`, authorisation. Coupler additions: router object (type 6),
   `PID_ROUTETABLE_CONTROL` function property, `A_FilterTable_Open/Read/Write`.
   Device profiles (mask, memory layout, objects, manufacturer, hardware type)
   come from the knxprod and `knx_master.xml`, which `knx-prod` already parses.
4. **Dumps and comparison**: device state as a stable text file; a diff of two
   dumps.
5. **knxcfg client**: a KNXnet/IP transport in `knx-transport` and the live
   download executor (incl. `LdCtrlWriteRelMem` and objects named by type),
   tested against the simulation.

## Steps

1. Tunnelling server and bus with frame log; ETS connects and finds a
   simulated device (ETS "Information" on a device: reads descriptor 0 and
   properties). **Done**, see "Step 1 results".
2. Simulated BE-02001.02 (mask 0705). Its tables are already known from the
   ETS5 capture, so it is the first check: ETS downloads, the dump must
   contain the known table bytes, then compare the parameter image.
   **Done**, see "Step 2 results".
3. Live download in `knxcfg` against the simulation; compare dumps.
   **Done**, see "Step 3 results".
4. Coupler (mask 091A, the IP router of the real project): filter table and
   procedure. ETS might want to configure an IP router over IP instead of
   through the bus; to be found out.
5. System B device and the partial download.
6. `Links` order: change one sending address in an ETS copy of the project,
   export, look at `@Links` (no emulator needed).

## Step 1 results

`knx-sim` serves search (multicast and unicast), description, one tunnelling
connection (link layer, basic and extended CRI), one device management
connection (`M_PropRead`/`M_PropWrite` on the interface's device object), the
heartbeat and the data repetitions. The bus gives a positive `L_Data.con` if
a device has the destination address, otherwise a negative one. A simulated
device has a Transport Layer connection (one partner, sequence numbers,
T_ACK/T_NAK, repetitions acknowledged again), DD0, memory (64 KiB of zeros),
a device object with properties, `A_PropertyDescription_Read`,
`A_Authorize_Request` (level 0 for every key), `A_ADC_Read` (error answer),
and in programming mode `A_IndividualAddress_Read/Write`; serial number
services always. No 6 s connection timeout and no repetition of unacknowledged
responses yet.

What ETS 5.7.4 does (log of the run on 2026-11-09):

- Search: `SEARCH_REQUEST_EXTENDED` (020Bh) and `SEARCH_REQUEST` in parallel;
  the simulation answers only the second, which is enough.
- "Test" and every later bus access: DESCRIPTION, tunnelling connection,
  then a device management connection, `M_PropRead` of the interface's
  `PID_DEVICE_DESCRIPTOR` (83, answered 091Ah) and `PID_MAX_APDULENGTH`
  (56, answered 15), then the management connection is closed. ETS shows
  "Ok" for the interface.
- Device info ("Geräteinfo") of 1.1.1 (mask 0705), connection-oriented:
  DD0; `A_PropertyValue_Read` object 5 PID 1 (twice); device object PIDs 12
  (manufacturer), 15 (order info), 11 (serial number), 78 (hardware type),
  25 (version); `A_ADC_Read` channel 1 with 8 reads (bus voltage); memory
  0060h (programming mode); object 3 PIDs 13 and 6, memory B6ECh; object 4
  PIDs 13 and 6, memory B6EDh; memory B6EAh and B6EBh. Without an answer to
  `A_ADC_Read` ETS disconnects after about 6 s and tries again.
- The memory addresses B6EAh–B6EDh and objects 3/4 are the System 7 load
  states and program versions: step 2 has to fill them for the BE-02001.02.
- Programming mode: `A_IndividualAddress_Read` broadcasts about every 3 s;
  the device in programming mode is listed.

## Step 2 results

**Simulated device.** `profile::device` builds a device from a knxprod: mask,
manufacturer and application from the application program, the hardware type
from the load procedure's `LdCtrlCompareProp` on PID 78 (the BE-02001.02 has 10
octets of inline data; the device answers the first six), objects 1–4 with
property based load state machines (`03_05_01 §4.23.2`, transitions of Table 94,
the recommended one where the table offers several), their load states
mirrored at the mask's memory mapped `…LoadStatus` addresses (B6EAh–B6EDh), the
run state, and the effects of the segment controls: a data segment of a table
sets `PID_TABLE_REFERENCE`, a task segment of the application sets
`PID_PROGRAM_VERSION` (the application id). No `PID_DEVICE_CONTROL`, like the
device in the capture below. The dump lists the objects with load state, load controls of the
current load and properties, and every memory octet a client wrote.

**Replay of a captured download.** `ETS5.log` of 2026-11-03 holds ETS's
download of a BE-02001.02 at 1.1.3 with the device's answers (raw cEMI). That
device was probably not MDT hardware but knx-core's emulation
(`devices/binary-input`: same application, hardware type `00 00 00 00 02 AC`,
max APDU length 254 from `libknx`; serial number not starting with 0083h).
So the capture is a reference for what ETS sends, not for how the original
device answers. The frames are extracted to `refs/ets5/be-02001-download.txt`
(untracked; it contains the serial number). `tests/ets5_replay.rs` sends ETS's
81 requests to the simulated device and compares all 79 frames it sends with
the captured device's: equal, except the serial number (not compared) and the
maximum APDU length (254, set in the test). This shows that knx-sim and the
knx-core emulation agree, not that either matches MDT hardware.

**Live ETS download.** ETS project "knx-sim" (new, only for the simulation):
BE-02001.02 at 1.1.1, default parameters, no group addresses. "Programmieren →
Applikationsprogramm" into the simulation: completed, all state columns green.
Same sequence as in the capture; memory writes in 12-octet chunks (the
interface's 15), and each write is read back. The empty tables are written as
their length octet only (01h at 4000h, 00h at 4201h).

**Comparison with knxcfg.** The project, exported by ETS and imported into
knxcfg, gives a download (`knxcfg download 1.1.1 --frames`) that was replayed
into a second simulated device. Diff of the two dumps:

- equal: load sequence, load states, application id, group object table
  (4400h–4467h);
- task segments of LSM 1 and 2: ETS sends PEI type and application id of the
  application (`03 02 40 00 01 00 83 00 B6 32`), knxcfg zeros;
- tables: knxcfg writes the whole segments including the knxprod template
  behind the table end (and 4001h–4002h); ETS writes the length octet and the
  used entries only, and never 4001h–4002h (with `AddressTable_Bcu1` the
  first entry is probably the device's own address, so knxcfg might overwrite
  it; to be checked);
- parameter segment 4468h–450Fh: 31 octets differ, and at every one ETS wrote
  the knxprod template byte of `AS-4468`. The same 31 offsets differ in the
  replay of the captured download. Likely knxcfg writes values (defaults) of
  inactive parameters where ETS keeps the template; to be confirmed per
  parameter. (The offsets agree with the capture because ETS, not the
  device, chose those bytes.)

These are the inputs for step 3. All ETS logs of the "test" project show
GO0's config octet as 4Bh (communication off) although GO0 is linked to
GA-1, while `docs/ets5-findings.md` and `oracle.rs` state 4Fh; resolved in
step 3 (project state, see there).

## Step 3 results

The three differences of step 2 are fixed in `knx-prod`:

- task segments carry the application's PEI type and id for every load state
  machine;
- a System 7 address or association table is allocated whole and written in
  its used part (`WriteMem` steps after the allocation): the address table's
  length octet and group addresses, skipping its first entry, and the
  association table;
- the parameter image writes only active parameters and parameters set by a
  reached `Assign` or a calculation; everything else keeps the template. Writing
  only active ones was not enough: `MD-2_P-26` ("send on falling edge after
  reset") is hidden but set from `MD-2_P-1` by an `Assign`, and ETS writes it.

Result: for the "knx-sim" project (default configuration) the dump of
knxcfg's download, replayed into a simulated device, equals the dump of ETS's
live download. For the "test" project (`test.knxproj`, with links and changed
parameters) against the captured download only GO0's configuration octet
differs: knxcfg 4Fh, ETS 4Bh. The export is 20 minutes older than that
download, and its `ComObjectRef` (`MD-2_O-3-0_R-11`) enables the
communication flag, so the flag was probably switched off in ETS in between.
Checked with a linked object: in the "knx-sim" project GO0 linked to 0/0/10
(knxprod flags), ETS writes 4Fh, like knxcfg, and the dumps of the two
downloads are equal again (tables included: address table length at 4000h
and 0/0/10 at 4003h, association table at 4201h).

**Live download in knxcfg.** `knx-transport::ip::IpTransport` is a KNXnet/IP
tunnelling client (the frame code moved from knx-sim to
`knx-transport::knxip`), and `knxcfg download <ia> --send -i <address>` runs
the plan with `download::execute`: it checks the device descriptor against
the application's mask before loading, compares `CompareProp` properties,
expects the load state of every load event (`03_05_01 §4.23.2.4`, waiting out
LoadCompleting), reads back every memory write, resolves objects named by
type and `LdCtrlWriteRelMem` addresses. Coupler filter tables are not
supported yet (step 4).

Result: `knxcfg download 1.1.1 --send -i 192.168.77.1` of the imported
"knx-sim" project (GO0 linked to 0/0/10) into a fresh simulated device:
24 steps, 0.4 s, and the dump equals the one of ETS's live download.
`download::live_tests` runs the same against knx-sim on localhost, and checks
that a device with another hardware type stops the download before
anything is loaded.

## Operating ETS

ETS has no known command-line way to start downloads, so someone has to click.
The VM is a plain QEMU process on the Linux host (Windows 7, 32 bit, ETS5;
the Claude desktop app does not run on 32-bit Windows). Two ways:

- **The user clicks** in ETS, and the logs and dumps are read on the Linux
  side.
- **The Linux Claude Code session clicks**, over QEMU's control socket (QMP),
  with `tools/vm.py`. Nothing is installed in the guest. Tested 2026-11-09
  on the desktop, the Run dialog and Notepad++, not yet in ETS.
  - The VM has to be started with
    `-qmp unix:$HOME/.win7.qmp,server=on,wait=off` (QMP cannot be added to
    a running VM) and keeps `-usbdevice tablet`, so the mouse is positioned
    absolutely.
  - `vm.py shot` writes the screen as a PNG (`screendump`, 1600x900);
    `vm.py click X Y`, `move`, `type TEXT` and `key COMBO` send input
    (`input-send-event`, `send-key`). Coordinates are screen pixels.
  - The guest uses the German keyboard layout. `type` translates characters
    for it; `key` names physical keys, so Ctrl+Z is `key ctrl-y`.
  - Each step is action, screenshot, check. Slow, fine for "connect to the
    emulator, download device X, export the project", poor for long
    parameter editing.
  - Type only into a window that was opened for the purpose and is seen in
    the last screenshot: the guest also has other windows open (Notepad++
    with files on the shared folder) and pop-ups such as update dialogs
    take the focus.
- Not recommended: scripting the ETS interface with Windows UI Automation
  (brittle).

## Rules

- ETS only ever has the emulator configured as its connection while testing;
  never the USB interface to the real bus.
- Use a **copy** of the project in ETS: ETS records download state in the
  project.
- No names, addresses or other contents of the real project in tracked files
  (logs and dumps of it stay in the scratch directory or under `refs/`, which
  is gitignored).

## VM network

Set up 2026-11-09. The VM has a second NIC on a host-only tap device, no
bridge; the first NIC (`-net user`, QEMU's NAT) stays for the internet and the
shared folder (`Z:`).

```
host:   ip tuntap add dev tap-knx mode tap user mw
        ip addr add 192.168.77.1/24 dev tap-knx
        ip link set tap-knx up
qemu:   -netdev tap,id=knx,ifname=tap-knx,script=no,downscript=no
        -device virtio-net-pci,netdev=knx
guest:  192.168.77.2/24, no gateway, no DNS
```

The emulator listens on `192.168.77.1:3671` and joins 224.0.23.12 on
`tap-knx`. Checked: ARP, ping from the guest to the host, and a UDP round
trip from a guest socket to the host's port 3671 and back. The guest does not
answer pings (Windows firewall). Not yet checked: multicast search, and
whether the Windows firewall lets replies through to ETS.

## Needed from the user

- ETS is ETS5 (installed in the VM); to check whether it uses tunnelling v1.
- The clicks in ETS, or starting the VM with the QMP socket described above.

## Other ways to record ETS (for later)

- With the USB interface passed from the Linux host into the VM, `usbmon` on
  the host sees every HID report ETS sends: a record of a real download
  without changing ETS. The VM already passes the interface through with
  `-device usb-host,...,pcap=/tmp/usb.pcap`, which records the same in QEMU.
- An instrumented build of the own firmware (`knx-core`) on a module, which
  emulates an MDT product ETS knows (`apps/demo-switch`, `devices/binary-input`),
  logging every management write. This confirms the result on real TP1
  hardware and may explain the open M4 hardware problem.
