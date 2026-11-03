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
D13 record knxprod path per device at `add` time.

## Spec / references
- `refs/spec` (untracked, copyrighted). Cite as **`<doc> §X.Y Title`**, e.g.
  `09_03 §3.4.1.2 KNX HID report header`. Index: `docs/spec-index.md`.
- Key docs: `09_03 §3 KNX USB Interface` · `03_06_03 §4.1 cEMI: message format
  and services` · `03_05_02 Management Procedures` · `03_05_03 Configuration
  Procedures (S-Mode)` · `03_05_01 Resources` · `03_04_01 AIL` · `03_07_02 DPT`.
- knxprod samples: `refs/knxprod/` (MDT binary inputs, MDT actuators, KLIC-MITT).
- Observed knxprod/`.knxproj` facts: `docs/knxprod-format.md`, `docs/plan.md`.

## Workspace (crates/)
- `knx-core` — typed model: `IndividualAddress`, `GroupAddress` (string
  serde), `Project`, `Area/Line/DeviceRef`, `GroupAddressEntry`,
  `DeviceInstance { parameters, links }`, `Project::sort()`.
- `knx-store` — YAML persistence, deterministic ordering.
  Files: `knx.yaml`, `topology.yaml`, `groups.yaml`, `devices/<ia>.yaml`.
- `knx-prod` — knxprod parser: catalog, hardware (h2p→app), application
  program (parameter types/params/unions, group objects, module defs),
  `Dynamic` tree (pages/`choose`/`when`/`Module`), `<Languages>` translations.
  M5: `<Code>` segments (absolute/relative, base64→bytes), `Memory/@BaseOffset`,
  `AddressTable`/`AssociationTable` headers, and `build_parameter_image` +
  `check_template` (device-level parameter overlay + template self-check).
  Module `BaseOffset` instantiation done: module parameters are overlaid at
  `argument base + Memory.Offset` for every `Module` instance (all `when`
  branches, since memory allocation is static).
- `knx-form` — form engine: `evaluate(ap, values, lang) -> Vec<FormPage>`
  (`FormPage { title, sections }`; `FormField { id, label, kind, value,
  enabled }`; `FieldKind::{Checkbox, Enum, Number, Text}`). Resolves
  visibility (`choose`/`when`), expands modules, localizes labels
  (translation → `@Text` → `@Name`), hides `Access="None"`, disables
  `Access="ReadOnly"`, substitutes `{{ChNo}}`/`{{0}}`. Module titles come
  from the instance `Name`, falling back to the module def's top-level
  `ParameterBlock` `Text` template (`{{ChNo}}` = the instance's `ChNo`
  argument — text or numeric — e.g. "Logic 1..4").
- `knx-transport` — stub only: `CemiFrame` encode/decode + `Transport` trait.
- `knxcfg` — CLI (`main.rs`) + TUI (`tui.rs`, ratatui): panes shown as a
  tab row; enums open a popup listing every value as `label (value)`.

## Commands (dir implicit except `init`; `-C <dir>` to override)
```
knxcfg init <dir>
knxcfg list
knxcfg group-add <ga> [--name N] [--dpt D]     group-rm <ga>
knxcfg add <knxprod> <h2p> <ia> [--name N]     rm <ia>
knxcfg link <ia> <object> <ga>                 unlink <ia> <object> <ga>
knxcfg device-set <ia> <param-id> <value>
knxcfg edit <ia> [--knxprod <file>]            # TUI: Tab=pages, Enter=edit, s=save, q=quit
knxcfg catalog <knxprod> | probe <knxprod> <h2p> | form <knxprod> <h2p>
knxcfg memory <knxprod> <h2p>                      # M5: segments, placements, template check
knxcfg tables <knxprod> <h2p>                      # M5: group-object table (ASAP + flags + vft)
knxcfg download <ia> [--knxprod <file>]            # M5 dry run: full image + plan + read-back self-check
```
`add` auto-creates Area/Line from the IA and records the canonical knxprod
path in `DeviceRef.knxprod` (absolute — known portability tradeoff).

## Milestones
Done: M0 skeleton · M1 knxprod parser · M1.5 form engine · M1.6 project
editing + TUI (incl. translations, access filtering, pages).
In progress: **M5** memory-image builder — code segments + device-parameter
overlay + template self-check done (`knxcfg memory`); module `BaseOffset`
instantiation done (`Argument/@Allocates` parsed); table builders done and
ETS5-oracle-checked against a real download (`docs/ets5-findings.md`, ETS's
`CommunicationTableFormatterSystem7`):
- **GrAT** = `[len:1][IA:2][GA:2×N]` (big-endian, ascending)
- **GrOAT** = `[len:1][TSAP:1][ASAP:1]×N`
- **GoT** = 4 octets/GO `[data_pointer][flags][00][vft]`, no length prefix;
  produced by *overlaying flags onto the knxprod template* (the data pointer
  is baked into the template, not computed).
- GO ASAP = `base + Number` (`BaseObj`/`ObjNumberBase`/`BaseNumber` args),
  verified: Eingang A=0–5, B=6–11, Logic=12–23, "In Betrieb"=24.
Done since: full device image assembly (`knx_prod::build_device_image`) —
parameter image + GrAT/GrOAT/GoT overlaid at their knxprod header offsets —
and the active-mode GO filter (`tables::active_group_objects`, choose/when
visibility + parameter values). The inverse read-back path is also in place:
`memory::decode_parameter_image`, `tables::read_links`, and
`read_back_device`. The knxprod `<LoadProcedures>` element is parsed and
`download_plan` now emits the device's exact load sequence (`LdCtrlConnect`,
`LdCtrlUnload/Load/AbsSegment/TaskSegment/LoadCompleted`, `LdCtrlRestart`,
`LdCtrlDisconnect`) with the assembled segment bytes attached.
`knxcfg download <ia>` is a dry run that prints the assembled segments, the
load-procedure plan, and a read-back self-check against the project values.
Remaining: encoding the plan steps as real `A_Memory_Write`/
`A_PropertyValue_Write` cEMI messages over the transport, device read-back,
and byte-identity vs ETS (oracle).
Next: **M2** `.knxproj` import (clean-room RE) · **M3** `.knxproj` export ·
**M4** USB cEMI transport (`09_03 §3`) · rest of **M5** (download + read-back,
byte-identical to ETS as oracle) · **M6** hardening.

## Known loose ends
- GO linking uses `object_number` only. The download path filters to the
  active mode via `tables::active_group_objects` (choose/when visibility +
  parameter values); `tables::group_objects` still exists for enumerating
  *all* possible GOs (`docs/ets5-findings.md`).
- knxprod path is stored absolute (machine-local); `--knxprod` override exists;
  vendoring into `vendor/` is a later option.
- Rust = Debian apt package (no rustup; leftover `~/.cargo` was removed).

## Build/test
`cargo build --offline` · `cargo test --offline` (29 tests).
Deps via apt (already installed): serde, serde_yaml, quick-xml(+serialize),
zip, base64, clap, anyhow, thiserror, ratatui 0.28, crossterm 0.27.
