# KNX Configuration Tool — Project Plan (draft)

> Status: draft for discussion. Open decisions are collected at the bottom.
> This is a living document — we update it as decisions are made.

## 1. Vision

Replace ETS for day-to-day KNX commissioning with an open, scriptable,
version-control-friendly tool.

Core value proposition:

- **Text-first project model** — a project is a directory of human-readable
  files that diff cleanly in git and merge sanely.
- **Deterministic** — same project in → same project out. No hidden state,
  no binary blobs in the repo (except explicitly vendored knxprod files).
- **Interoperable** — import/export ETS project files (`.knxproj`) so the tool
  can sit alongside ETS in existing workflows.
- **Reproducible download** — build the exact device memory image from the
  project + knxprod and push it to a device, byte-for-byte identical to what
  ETS would produce (that is the long-term goal).

## 2. Non-goals (for now)

- No full ETS-style GUI. CLI for everything structural; an interactive
  *device parameter editor* is required (see D3) because knxprod defines
  dynamic per-device forms.
- No bus monitoring / group telegrams / visualisation (that is a different
  tool and can reuse the transport layer later).
- No ETS plugin/host integration.
- No support for ETS's "Building/Functions" structure editing beyond what is
  needed for round-tripping (see decision D7).
- No RF/PLC commissioning initially — TP and IP devices only.

## 3. Domain primer (what we are dealing with)

Terminology (this is the vocabulary the whole plan uses):

| Term | Meaning |
|---|---|
| **IA** / physical address | 16-bit `Area.Line.Device` (e.g. `1.1.1`), identifies a device on the bus |
| **GA** / group address | 16-bit `Main/Middle/Sub` (3-level) or `Main/Sub` (2-level), the communication channel |
| **GO** / group object / ComObject | A device's communication endpoint; linked to GAs via the association table |
| **DPT** | Datapoint type — defines the wire format + semantics of a GO (e.g. `DPT 1.001` switch) |
| **Application program** | The device firmware's configurable "personality": parameters, group objects, memory layout |
| **knxprod** | Manufacturer product database (ZIP + XML). Describes hardware, application programs, parameters, datapoint types |
| **ETS project file** | `.knxproj` — ZIP container, `project.xml` + per-device instance files (`P-XXXX/0.xml`) |
| **Download / programming** | Writing the device's memory (parameters + group-object table + address table) over the bus |

## 4. Architecture

```
┌────────────────────────────────────────────────────────────────┐
│                     CLI (clap) + parameter editor             │
│  commands: init/add/rm, link/unlink, download, export/import   │
│  interactive: device parameter editor (driven by form engine)  │
├────────────────────────────────────────────────────────────────┤
│                        project (model)                         │
│   in-memory typed model of topology, IAs, GAs, GOs, params    │
├──────────────────────┬──────────────────────┬──────────────────┤
│   persistence        │   interop            │   commissioning  │
│   (git-friendly      │   knxprod parser     │   memory image   │
│    load/save)        │   etsproj import/    │   builder        │
│                      │   export             │                  │
├──────────────────────┴──────────┬───────────┴──────────────────┤
│              form engine        │                              │
│   knxprod → dynamic form model  │                              │
│   (visibility/enable rules)     │                              │
├─────────────────────────────────┴──────────────────────────────┤
│                     protocol / transport                       │
│   cEMI · management procedures (mem r/w, prop r/w, restart)    │
│   USB cEMI tunnel (first) · KNXnet/IP (later) · FT1.2 (later)  │
└────────────────────────────────────────────────────────────────┘
```

Layers are strict: `persistence`, `interop`, and `commissioning` all depend
only on `project` (the typed model). The CLI orchestrates. This keeps the
model the single source of truth and makes import/export pure functions
(model ↔ file).

## 5. Data model (typed model)

```
Project
 ├─ Metadata (name, version, addressing style, format version)
 ├─ Topology
 │    └─ Area ─ Line ─ Device (IA) 
 │         └─ hardware/application ref → knxprod
 │         └─ instance: parameters, GO links, device name/description
 ├─ GroupAddresses (hierarchical, 2- or 3-level)
 │    └─ GA ─ datapoint type, flags, description
 └─ (optional) Trades/building structure — see D7
```

Key invariant: **the typed model is canonical**. Every file format
(knxprod, .knxproj, our own) is just a serialization boundary.

## 6. Persistence (git-friendly project format)

Proposed: **a directory tree of YAML files** with deterministic key order and
stable sorting. One file per concern keeps diffs small and merges clean.

```
project/
├─ knx.yaml                 # project metadata, addressing style, format version
├─ topology.yaml            # areas/lines/devices with IAs
├─ group-addresses.yaml     # GA tree (sorted deterministically)
├─ devices/
│   └─ 1.1.1.yaml           # device instance: hardware ref, parameters, GO links
└─ vendor/
    └─ my-manufacturer.knxprod   # vendored product data (optional, git-lfs)
```

Why YAML: human-readable, diff-friendly, comments, good multi-language
support. (JSON/TOML are alternatives — see decision D2.)

Rules for git-friendliness:

- Deterministic ordering everywhere (sort addresses numerically).
- No timestamps/uuids in the working tree unless they carry meaning.
- IDs must be stable and semantic (e.g. `1.1.1`), not ETS's opaque GUIDs.
- Parameters stored by symbolic name + value, not raw memory offsets.

## 7. Interop

### 7.1 knxprod (product database)

- Parse the ZIP + `catalog.xml` (namespace `http://knx.org/xml/project/…`).
- Extract for each device: hardware (mask version), application programs,
  parameter blocks + parameter types, com-object tables, DPT references.
- This gives us everything needed to (a) render parameters, (b) build the
  memory image, (c) validate GO links.

### 7.2 ETS project (`.knxproj`)

- **Import**: `.knxproj` → typed model → our format. Resolve knxprod
  references from the local product store.
- **Export**: our format → typed model → `.knxproj`. Goal: ETS can open it
  and download the result identically.
- This is the piece that requires the most reverse-engineering effort, and
  it is where the clean-room discipline matters most.

## 8. Protocol / transport

- **USB cEMI tunnel** first — PC talks raw cEMI over USB (no virtual COM
  port, no FT1.2), device is connected straight to the bus (see D4).
  Specified in `09_03 §3 KNX USB Interface` (see `docs/spec-index.md`).
- KNXnet/IP tunneling and FT1.2/TP-UART later.
- cEMI framing + management procedures per KNX specifications
  (`03_06_03 §4.1 cEMI: message format and services`; `03_05_02`;
  `03_05_03`), e.g. `Memory_Read`, `Memory_Write`, `Property_Read`,
  `Property_Write`, `Restart`, `IndividualAddress_Write`,
  `DeviceDescriptor_Read`. Full citations in `docs/spec-index.md`.

The bus/protocol layer is fully specified by the KNX system specifications
(ISO/IEC 14543-3) — this part is **not** reverse engineering. The knxprod
XML schema (XSD) ships with the KNX Manufacturer Tool, not these PDFs;
`.knxproj` is ETS-proprietary (see `docs/spec-index.md`).

## 9. Commissioning / download flow

1. Resolve device → knxprod application program.
2. Build parameter memory image (parameter block layout from knxprod).
3. Build group-object table, address table, and association table from GOs/GAs.
4. Merge into the device memory map per the application program's load
   procedure (this is described in the knxprod data).
5. Download via management procedures (with verify = read-back).
6. Optionally: program IA first, then restart.

Long-term correctness bar: byte-identical memory image to ETS for the same
project+device. That is our test oracle.

## 10. Clean-room reverse engineering process

Two distinct roles (can be two people, or the same person in two strictly
separated passes):

1. **Analyst** — observes ETS (network capture, file inspection, black-box
   behavior), writes a **specification document** (what bytes/fields mean,
   what sequences happen, edge cases). Never shares decompiled code.
2. **Implementer** — reads only the specification (+ published KNX specs),
   implements against it. Never looks at ETS internals.

Ground rules:

- Prefer the published KNX specifications for everything they cover.
- The **entire wire path is documented**: cEMI, management procedures, and
  the USB transport are all in the KNX system specifications. Nothing to
  reverse engineer here.
- Reverse engineer only: `.knxproj` internal structure, exact download
  sequences ETS uses, quirks/undocumented behavior.
- Keep all analyst notes + captures in a separate repo (or `analysis/`)
  that is **never** a dependency of the implementation.
- Every format/protocol fact lands in a spec doc first, then code.

## 11. Milestones

1. **M0 — Skeleton**: repo, CLI, typed model, git-friendly persistence,
   unit tests. *(done)*
2. **M1 — knxprod parser**: read a real manufacturer knxprod, dump parameters
   and GO tables as text. Validated against several vendors. *(done —
   `knxcfg probe <file> <h2p-ref>` parses parameter types, parameters, unions,
   group objects and module definitions; verified on MDT binary inputs,
   MDT actuators and KLIC-MITT.)*
3. **M1.5 — form engine**: parse the `Dynamic` tree and evaluate visibility
   (`choose`/`when`) + module expansion → neutral form model. *(done —
   `knxcfg form <file> <h2p-ref>` prints the resolved parameter dialog with
   defaults; placeholder substitution for `{{ChNo}}`/`{{0}}`.)*
4. **M1.6 — project editing + TUI**: git-like implicit project dir, CLI
   commands to add devices/GAs/links/params, and an interactive ratatui
   parameter editor. *(done — `knxcfg add|rm|group-add|group-rm|link|unlink|
   device-set|edit`; `edit` re-evaluates visibility live, records the
   knxprod path at `add` time, shows localized labels, hides `Access="None"`
   params, and navigates pages with Tab/Shift-Tab.)*
4. **M2 — ETS project import**: `.knxproj` → model → our format. Round-trip
   stability (import twice = identical).
5. **M3 — ETS project export**: our format → `.knxproj`, opens in ETS.
6. **M4 — Transport**: USB cEMI tunnel (`09_03 §3 KNX USB Interface`), cEMI,
   management procedures. Able to read device descriptor + memory from a
   real device.
7. **M5 — Download**: build + write parameter/GO/address memory image,
   verify by read-back. Oracle test vs ETS (same project → same bytes).
8. **M6 — Hardening**: IA programming, restart handling, error recovery,
   multiple transports, CI with captures.

## 12. Decisions

Recorded (D1–D10):

| # | Decision | Choice |
|---|---|---|
| D1 | Language | **Rust** |
| D2 | Project format | **YAML** (directory layout, deterministic ordering) |
| D3 | UI surface | **Split**: CLI + interactive device parameter editor (**TUI/ratatui**) |
| D4 | First transport | **USB cEMI tunnel** (raw cEMI over USB, bus-connected) |
| D5 | ETS oracle | **ETS5** |
| D6 | knxprod version | **Latest (ETS5 catalog namespace)** |
| D7 | Buildings/Functions | **Keep + round-trip** |
| D8 | DPT library | **Parse from knxprod** |
| D9 | Addressing | **3-level (`a/b/c`) first** |
| D10 | MVP download | **Full** (IA + params + GO table + address table) |
| D11 | Dependencies | **Debian-packaged crates only**, offline build via `.cargo/config.toml` (directory source `/usr/share/cargo/registry`) |
| D12 | Project location | **Implicit** (walk up from cwd like git; `-C/--project` to override) |
| D13 | Product data ref | **Record knxprod path per device** at `add` time (so `edit`/`download` don't need it again) |

Still open:

- **D4a** — which KNX spec document covers the USB transport (so we can
  obtain and implement from it).
- **D4b** — USB class (HID vs bulk) and max cEMI frame size for the
  concrete interface you own.
