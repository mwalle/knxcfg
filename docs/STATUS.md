# Status — next-start prompt

`knxcfg` — a git-friendly KNX configuration tool to replace ETS.
Repo: `/home/mwalle/repos/knx/knx-config`. Rust. Build with **`cargo build --offline`**
(Debian crates only; see `.cargo/config.toml`).

## Goal
Configure GAs/IAs and download parameters to devices. Project files must be
git-friendly. Import/export ETS `.knxproj`. Clean-room reverse engineering
only where the KNX spec doesn't cover it.

## Decisions (locked)
D1 Rust · D2 YAML dir layout (deterministic) · D3 CLI + TUI (ratatui) ·
D4 USB cEMI tunnel transport · D5 ETS5 oracle · D6 knxprod ETS5 namespace ·
D7 keep Buildings/Functions round-trip · D8 DPTs from knxprod · D9 3-level
addressing · D10 full download · D11 Debian-packaged crates only (offline) ·
D12 implicit project dir (walk up like git, `-C/--project` override) ·
D13 record knxprod path per device at `add` time ·
D14 product store: project `vendor/` (default) + global
`~/.local/share/knxcfg/knxprod`, auto-resolved by h2p
(`import-product` copies into vendor/, `--global` into the global store).

## Spec / references
- `refs/spec` (untracked, copyrighted). Cite as **`<doc> §X.Y Title`**, e.g.
  `09_03 §3.4.1.2 KNX HID report header`. Index: `docs/spec-index.md`.
- Key docs: `09_03 §3 KNX USB Interface` · `03_06_03 §4.1 cEMI: message format
  and services` · `03_05_02 Management Procedures` · `03_05_03 Configuration
  Procedures (S-Mode)` · `03_05_01 Resources` · `03_04_01 AIL` · `03_07_02 DPT`.
- knxprod samples: `refs/knxprod/` (MDT binary inputs, MDT actuators, KLIC-MITT).
- Observed knxprod/`.knxproj` facts: `docs/knxprod-format.md`, `docs/plan.md`.

## Workspace (crates/)
- `knx-core` — typed model: `IndividualAddress` (reserved `x.y.0` /
  `15.15.255` rejected for devices), `GroupAddress` (3-level storage; parses
  3-level, 2-level `main/sub` and free `raw`), `Project`,
  `Area/Line/DeviceRef`, `GroupAddressEntry`, `DeviceInstance { parameters,
  links }`, `GroupObjectLink { object_number, ga, send }`, `Project::sort()`.
- `knx-store` — YAML persistence, deterministic ordering; `save` removes the
  files of devices that are no longer in the project.
  Files: `knx.yaml`, `topology.yaml`, `groups.yaml`, `devices/<ia>.yaml`.
- `knx-prod` — knxprod parser and download builder:
  - `eval` — **the** parameter evaluator used by form, memory and tables:
    all matching `when`s, comparison tests, `ParameterRef/@Value` defaults,
    `Assign`, `ParameterCalculation` (small JS subset, `calc.rs`), active
    union members, active `ComObjectRef`s and module instances. Keys:
    `P-3` / `MD-2_M-2/MD-2_P-26` in projects (app-id prefix stripped,
    `short_key`/`full_key`).
  - `memory` — parameter image; strict value encoding (enumerations, ranges,
    field widths, DPT 9 / IEEE floats, time, color, IPv4, binary values),
    little-endian option, active alternatives win shared memory.
  - `tables` — System 7 (mask 0701/0705, memory mapped, ETS5-confirmed) and
    System B (mask 07B0, property based, spec-derived) GrAT/GrOAT/GoT;
    sending GA first; `MaxEntries` and field-width checks.
  - `device_image` — full image + `download_plan` for `ProductProcedure`
    and `MergedProcedure` (mask procedure from `knx_master.xml`, `AppliesTo`,
    `choose` in load procedures, `RelSegment`/`WriteRelMem`/`WriteProp`/
    `LoadImageProp`/`Delay`/`MapError`); unknown load controls are errors.
  - `master` — `knx_master.xml`: mask procedures, datapoint types.
- `knx-form` — form engine on top of `eval`: `evaluate(ap, values, lang)`,
  `validate(ap, id, value)`, `is_editable`. `FieldKind::Checkbox` only for
  `UIHint="CheckBox"`; `ParameterRef` `Text`/`SuffixText` overrides; fields
  set by `Assign` are read-only.
- `knx-transport` — `CemiFrame`, `cemi::l_data_req` (TPCI and APCI share
  the first TPDU octet), `apdu::*` (`A_Memory_Read/Write`,
  `A_PropertyValue_Read/Write` with 12-bit start index, `A_Restart`, …),
  `Transport` trait, and (M4):
  - `usb` — KNX USB HID framing (`09_03 §3.4`: report ID 01h, sequence
    number/packet type, 8-octet transfer header in the start packet, up to
    5 reports, reassembly with the spec's error handling), Device Feature
    services (`09_03 §3.5.3`), `UsbTransport<D: HidDevice>`: on open it reads
    the supported EMI types, requires cEMI and sets it as active EMI type,
    tracks the bus connection status (Get + Info frames), then sets
    `PID_COMM_MODE` = Data Link Layer with a local `M_PropWrite.req`
    (`03_06_03 §4.2.2.4.2`: the mode is undefined until the client sets it).
    `KNXCFG_TRACE=1` dumps every HID report to stderr.
  - `hidraw` (Linux) — `/dev/hidrawN` via `libc::poll`; enumeration from
    sysfs; KNX interfaces are recognised by their report descriptor
    (report ID 1, 63-octet input + output, vendor usage page), nothing is
    sent to other HID devices.
  - `cemi::LData` — parsed `L_Data.ind/.con` (Confirm flag, TPCI, seq, APDU).
  - `mgmt` — `Connection`: `L_Data.con` per request (negative = error),
    connection-oriented TL client (`03_03_04 §5`, Style 1: T_ACK/T_NAK, 3 s
    ack timeout, 3 repetitions, re-ACK of repeated PDUs, T_NAK on a wrong
    sequence number, remote T_Disconnect), `device_descriptor_read`,
    `memory_read` (12-octet chunks).
- `knxcfg` — CLI (`main.rs`, `product_store.rs`, `download.rs` plan → cEMI
  frames) + TUI (`tui.rs`; typed values are validated before they are taken).

## Commands (dir implicit except `init`; `-C <dir>` to override)
```
knxcfg init <dir>
knxcfg list
knxcfg import-product <file...> [--global]    # copy knxprod into vendor/ (default) or global store
knxcfg catalog [<file>]                        # list store products (or one file)
knxcfg group-add <ga> [--name N] [--dpt D]     group-rm <ga> [--force]
knxcfg add <h2p> <ia> [--name N] [--knxprod F] rm <ia>
knxcfg link <ia> <object> <ga> [--send]        unlink <ia> <object> <ga>
knxcfg device-set <ia> <param> <value>         # validated; P-3 or MD-2_M-2/MD-2_P-26
knxcfg edit <ia> [--knxprod <file>]            # TUI: Tab=pages, Enter=edit, s=save, q=quit
knxcfg probe <h2p> | form <h2p> | memory <h2p> | tables <h2p>   [--knxprod <file>]
knxcfg download <ia> [--knxprod F] [--partial] [--frames]   # dry run: image, tables, plan
knxcfg interfaces                              # hidraw devices, KNX USB interfaces marked
knxcfg device-info <ia> [-i /dev/hidrawN]      # mask version (DD0) over the bus
knxcfg mem-read <ia> <addr> <count> [-i ...]   # hex dump; addr 0x4000 / 4000h / decimal
```
Bus commands use `--interface` or the only KNX `hidraw` device. `/dev/hidraw*`
is root-only by default; a udev rule (`SUBSYSTEM=="hidraw",
ATTRS{idVendor}=="<vid>", TAG+="uaccess"`) gives the logged-in user access.
knxprod files are resolved by h2p: `--knxprod` override → the path recorded at
`add` time (D13; relative to the project when the file is inside it) →
project `vendor/` → global store (`$XDG_DATA_HOME/knxcfg/knxprod`, default
`~/.local/share/knxcfg/knxprod`). `add` auto-creates Area/Line from the IA.
`link` checks that the object exists, the GA is in `groups.yaml` and is not
`0/0/0`, and warns on a DPT mismatch or an inactive object.

## Milestones
Done: M0 skeleton · M1 knxprod parser · M1.5 form engine · M1.6 project
editing + TUI.
**M5** (download builder): the parameter image, tables and plan are built
for all five sample products (System 7 and System B). The System 7 tables are
checked against the ETS5 oracle (`crates/knx-prod/tests/oracle.rs`); the
parameter image and the System B tables are spec/knxprod-derived and not yet
compared with an ETS download. The plan is encoded as cEMI frames
(`download --frames`) except `LdCtrlWriteRelMem`, which needs the device's
`PID_TABLE_REFERENCE` at run time.
**M4** (USB transport, in progress): USB HID framing, `hidraw`, the EMI
handshake, the TL connection and DD0/memory reads are implemented and
tested against scripted mocks. First hardware run: Device Feature
handshake OK, but no `L_Data.con` for `T_Connect` (cEMI mode not set; fixed
by `PID_COMM_MODE`, re-test pending). Still to do: a working hardware run,
`A_PropertyValue_Read/Write` responses, `NM_IndividualAddress_Read/Write`,
executing a download plan (incl. `LdCtrlWriteRelMem`).
Next: finish **M4** with a hardware run, then live download/read-back ·
**M2** `.knxproj` import (also the way to get ETS parameter images to compare
against) · **M3** `.knxproj` export · **M6** hardening.

## Known loose ends
- Not supported (reported, not silent): KNX Data Secure (download only works
  with security off), `Allocator`-based module arguments, `<Property>`
  parameters other than the device hardware checks, calculations outside
  the JS subset (KLIC's Steinhart-Hart tables), coupler/IA programming.
- Unverified against ETS: System B table bytes, the partial download, the
  communication flag of unlinked System B objects, rounding of calculation
  results into integer parameters, the content of table segments behind the
  table end (the template residue is kept).
- Rust = Debian apt package (no rustup; leftover `~/.cargo` was removed).

## Build/test
`cargo build --offline` · `cargo test --offline` (79 tests, incl. the ETS5
oracle regression test that reads `refs/knxprod`).
Deps via apt (already installed): serde, serde_yaml, quick-xml(+serialize),
zip, base64, clap, anyhow, thiserror, ratatui 0.28, crossterm 0.27.
