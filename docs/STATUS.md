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
```
`add` auto-creates Area/Line from the IA and records the canonical knxprod
path in `DeviceRef.knxprod` (absolute — known portability tradeoff).

## Milestones
Done: M0 skeleton · M1 knxprod parser · M1.5 form engine · M1.6 project
editing + TUI (incl. translations, access filtering, pages).
Next: **M2** `.knxproj` import (clean-room RE) · **M3** `.knxproj` export ·
**M4** USB cEMI transport (`09_03 §3`) · **M5** memory-image builder + download
(byte-identical to ETS as oracle) · **M6** hardening.

## Known loose ends
- GO linking uses `object_number` only; module GO identity needs the
  association-table work (M5).
- knxprod path is stored absolute (machine-local); `--knxprod` override exists;
  vendoring into `vendor/` is a later option.
- Rust = Debian apt package (no rustup; leftover `~/.cargo` was removed).

## Build/test
`cargo build --offline` · `cargo test --offline` (10 tests).
Deps via apt (already installed): serde, serde_yaml, quick-xml(+serialize),
zip, base64, clap, anyhow, thiserror, ratatui 0.28, crossterm 0.27.
