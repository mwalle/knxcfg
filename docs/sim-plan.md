# knx-sim: verifying downloads against ETS without real devices

Status: planned, nothing implemented (2026-11-09). The work starts with step 1
below.

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
   properties).
2. Simulated BE-02001.02 (mask 0705). Its tables are already known from the
   ETS5 capture, so it is the first check: ETS downloads, the dump must
   contain the known table bytes, then compare the parameter image.
3. Live download in `knxcfg` against the simulation; compare dumps.
4. Coupler (mask 091A, the IP router of the real project): filter table and
   procedure. ETS might want to configure an IP router over IP instead of
   through the bus; to be found out.
5. System B device and the partial download.
6. `Links` order: change one sending address in an ETS copy of the project,
   export, look at `@Links` (no emulator needed).

## Operating ETS

ETS has no known command-line way to start downloads, so someone has to click.
Options, from simplest:

- **The user clicks** in ETS, and the logs and dumps are read on the Linux
  side.
- **Computer use in the VM**: install the Claude desktop app inside the Windows
  VM, enable Settings -> This computer -> Computer use (not confirmed that the
  Windows app offers it), approve ETS as a controlled app. That session clicks
  in ETS. Slow and coarse: fine for "connect to the emulator, download device
  X, export the project", poor for long parameter editing.
- **Driving the VM session from the Linux Claude Code session**: the Linux
  session can message Claude sessions on the same machine, and sessions on
  other machines only when Remote Control is connected. Not tested with a
  desktop app session inside the VM. To try: start the VM session, connect
  Remote Control on the Linux side, and check the list of reachable sessions
  there (ListAgents). The VM session is a separate Claude with its own context
  and approvals, so instructions have to be complete.
- Not recommended: scripting the ETS interface with Windows UI Automation over
  SSH (brittle).

## Rules

- ETS only ever has the emulator configured as its connection while testing;
  never the USB interface to the real bus.
- Use a **copy** of the project in ETS: ETS records download state in the
  project.
- No names, addresses or other contents of the real project in tracked files
  (logs and dumps of it stay in the scratch directory or under `refs/`, which
  is gitignored).

## Needed from the user

- VM network that reaches the Linux host on UDP 3671 (bridged or host-only).
  Multicast search may not cross the VM network; then the interface is added
  in ETS by IP address.
- The ETS version (ETS 5 or 6; ETS 6 may expect tunnelling v2).
- The clicks in ETS, or the computer-use setup described above.

## Other ways to record ETS (for later)

- With the USB interface passed from the Linux host into the VM, `usbmon` on
  the host sees every HID report ETS sends: a record of a real download
  without changing ETS.
- An instrumented build of the own firmware (`knx-core`) on a module, which
  emulates an MDT product ETS knows (`apps/demo-switch`, `devices/binary-input`),
  logging every management write. This confirms the result on real TP1
  hardware and may explain the open M4 hardware problem.
