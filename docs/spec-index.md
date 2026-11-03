# KNX specification index

> Citation convention: **`<document> §<section> <title>`**, e.g.
> `09_03 §3.4.1.2 KNX HID report header`.
> `document` is the spec filename (without the `.pdf` suffix).
> `refs/` is never committed (external, possibly copyrighted documents).

Spec version: `refs/spec/v3.0.0/` (KNX Specifications v3.0).

## Transport — USB cEMI tunnel (D4)

Source: `09_03 Basic and System Components - Couplers`

- `09_03 §3 KNX USB Interface` — overall USB interface model
- `09_03 §3.3.2.1 HID Class` — device class used
- `09_03 §3.4 KNX USB HID class frames`
- `09_03 §3.4.1 HID report frame`
- `09_03 §3.4.1.2 KNX HID report header`
- `09_03 §3.4.1.3 Data (KNX HID report body)` — incl. protocol version / identifiers
- `09_03 §3.5 KNX USB bus access device requirements` — level 1 (USB) requirements
- `09_03 §3.4.1.3.4 Protocol identifiers` — protocol ID / EMI ID coding
- `09_03 §3.5.3.2 Device feature services` — Get/Response/Set/Info
- `09_03 §3.5.3.3 Device features` — supported/active EMI type, bus
  connection status

## Transport Layer (connection-oriented)

Source: `03_03_04 Transport Layer`

- `03_03_04 §4 Parameters of Transport Layer` — 6 s connection timeout,
  3 s acknowledgement timeout, `max_rep_count` 3
- `03_03_04 §5 State Machine of Connection-Oriented Communication Mode` —
  events E00–E27, actions A0–A15 (Style 1 implemented)

## cEMI framing

Source: `03_06_03 EMI_IMI`

- `03_06_03 §4.1 cEMI: message format and services`
- `03_06_03 §4.1.5.3.4 L_Data.con` — Confirm flag (`Ctrl1` bit 0, 1 = error)
- `03_06_03 §4.2.2.4.2.2 cEMI communication mode "cEMI Transport Layer"`
- `03_06_03 §4.3 KNX extraneous information on cEMI connection` — protocol ID
  field in the host-protocol header

## Management procedures (IA, memory, property access)

Source: `03_05_02 Management Procedures`

- `03_05_02 §2 Network Management Procedures`
- `03_05_02 §2.2 NM_IndividualAddress_Read`
- `03_05_02 §2.3 NM_IndividualAddress_Write`
- `03_05_02 §3 Device Management Procedures` — memory read/write,
  property read/write, restart, etc.

## Configuration procedures (S-Mode download)

Source: `03_05_03 Configuration Procedures`

- `03_05_03 §1 Common requirements`
- `03_05_03 §2 Common procedures`
- `03_05_03 §3 S-Mode`
- `03_05_03 §1.5.4 Integration in the S-Mode Configuration Procedures`

## Device resources / interface objects

Source: `03_05_01 Resources`

- `03_05_01 §4 Device Resources` — Group Address Table Object, Association
  Table Object, Application Program Object, Group Object Table Object, …
- `03_05_01 §3 Network Resources`

## Application program structure / load procedure

Source: `03_04_01 Application Interface Layer` — sections to be indexed as
used (parameter blocks, memory layout, load procedure).

## Datapoint types

Source: `03_07_02 Datapoint Types` — sections to be indexed as used.

## KNXnet/IP (later transport)

Source: `03_08_xx` — `03_08_04 Tunnelling`, `03_08_03 Management`,
`03_08_02 Core`; index when we reach M4+ IP support.

## NOT in these PDFs (note)

- **knxprod XML schema (XSD)** — the normative schema ships with the KNX
  Manufacturer Tool, not in these PDFs. We obtain/observe the XSD (and real
  knxprod files) separately; knxprod is a KNX-standardized format.
- **ETS `.knxproj` format** — ETS-proprietary, not in the KNX specs. This is
  the reverse-engineering target (clean-room process in `docs/plan.md` §10).
