# Code review — knxcfg (2026-11-03)

Scope: the whole workspace (~4.6k lines of Rust + `docs/`), checked against
the KNX spec in `refs/spec` (`03_05_01` Resources, `02_03_01` Load Controls,
`06` Profiles) and the five sample knxprods in `refs/knxprod`. The tool was
built and run; it builds cleanly and all 29 tests pass. Findings marked
**(reproduced)** were confirmed by running `knxcfg` in a scratch project.

## Verdict

The structure is good: clean crate layering, deterministic YAML, honest
STATUS docs, and checking bytes against an ETS capture is the right approach.
But the image is currently only right for one device, with default
parameters. Three problems break it for most real configurations:

1. Visibility (which parameters and objects are active) is evaluated wrongly
   in several ways.
2. Several knxprod rules that set parameter values are ignored.
3. Only one load-procedure style and one table format (mask 0705) are
   supported.

3 of the 5 sample products can't be downloaded at all.

## Status after the fixes (2026-11-03)

| # | Finding | Status |
|---|---------|--------|
| 1–5 | `when` semantics, comparisons, unions, `ParameterRef/@Value`, `Assign` | Fixed: one evaluator, `crates/knx-prod/src/eval.rs`, used by form, memory and tables. `ParameterCalculation`: JS subset in `calc.rs`; scripts outside it are reported |
| 6 | GoT layout | Fixed: spec layout (`§4.18.9.1`); docs corrected (AS-0700 = RAM) |
| 7 | Load procedures | Fixed: `MergedProcedure` with mask procedure from `knx_master.xml`, `AppliesTo`, `choose` in load procedures; unknown load controls are errors. All five samples plan |
| 8 | Download steps | Fixed: `TaskSegment` carries PEI type + app id; RAM/stack segments allocate only. **Correction:** the property-based `AbsSegment` event carries a *length* (`03_05_02 §3.31.3.4`); end addresses only appear in the legacy S19 load records. The size field was right |
| 9 | Silent value corruption | Fixed: strict encoding; float/time/color/binary types; CLI and TUI validate |
| 10 | Sending GA | Fixed: `link --send`, first association |
| 11 | Link validation | Fixed: object, GA existence, `0/0/0`, DPT; `group-rm` refuses (or `--force`); dropped links are reported |
| 12 | Table overflow | Fixed: `MaxEntries` and field-width checks |
| 13 | `rm` | Fixed: `save` removes stale device files |
| Medium | duplicated visibility, self-check, template check, keys, bare-id fallback, Priority/ComObjectRef overrides/Allocators/Property, checkbox, paths, ASAP docs | Fixed or reported (Allocators, non-device-check `<Property>` parameters, segment `<Mask>` parsed only) |
| Low | transport, IA programming, 2-level GA, DPTs, Data Secure, reserved IAs | APDU/TPCI encoding fixed, 2-level/free GA input, DPT validation, reserved IAs, Data Secure warning. USB HID transport and IA programming are still open |

Verification: `crates/knx-prod/tests/oracle.rs` reproduces the ETS5 tables
byte for byte. The parameter image and the System B tables are derived
from the knxprod and the spec; they still need an ETS download to compare
against (see `docs/STATUS.md`, "Unverified against ETS").

## Critical: wrong bytes or failure on real devices

### 1. Only the first matching `when` is used

`crates/knx-prod/src/tables.rs:507-523`, `crates/knx-form/src/lib.rs:284-296`

- ETS turns on every `when` whose test matches, and uses `default` only if
  none match.
- The oracle device's own app (`M-0083_A-00B6-32-9642`) has 44 `choose`
  blocks with overlapping tests, e.g. `['0 1 2 3', '0 1', '0 2 4', '1 3', …]`.
  That layout only works if all matches apply (KLIC: 18 such blocks).
- Effect: parameters and group objects go missing, so GoT flags are wrong and
  links are silently dropped.

### 2. Comparison tests in `when` are not supported

`crates/knx-prod/src/tables.rs:521` (and the form copy)

- `test` can be `!=0`, `>0`, `>=12`, `<…`. Relational tests per sample app:
  2 (oracle `00B6`) up to 298 (KLIC).
- Exact string matching never matches these, so the code falls through to the
  default branch.

### 3. Union members overwrite each other

`crates/knx-prod/src/memory.rs:464-484`, `memory.rs:299-309`

- All alternatives of a `<Union>` are written, so the last one wins.
  `knxcfg memory` shows `MD-2_UP-45 … UP-125` all placed at byte 16, mixing
  8, 16 and 32-bit values **(reproduced)**.
- Only the active member should be written: the visible one, or the one marked
  `DefaultUnionParameter="true"` (74 such markers in the samples).
- Union members' own `@Offset`/`@BitOffset` are not parsed (0 in this file,
  but not in general).
- Unions at device level (outside modules) are skipped entirely.
- Read-back decodes every member, so it can't tell which one is live.

### 4. `ParameterRef/@Value` is ignored

`crates/knx-prod/src/app.rs:170-178`

- The oracle app has 460 refs with a `Value=` override. Defaults shown in the
  form, the image, and the "minimal YAML" comparison in `cmd_edit` all use
  `Parameter/@Value` instead.
- `ParameterRef @Text` / `@SuffixText` overrides are ignored too.

### 5. `Assign` is ignored

`crates/knx-form/src/lib.rs:259`, `crates/knx-prod/src/dynamic.rs`

- 16 in the oracle app, 3272 in KLIC. Assigns copy or force parameter values,
  so the memory comes out wrong once a user changes the source parameter.
- `ParameterCalculation` and `Script` are also unhandled.

### 6. The Group Object Table layout is misunderstood

`crates/knx-prod/src/tables.rs:595-607, 761-812`, `docs/ets5-findings.md`,
`docs/OPEN-QUESTIONS.md`

The bytes come out right only by coincidence.

- For mask 0701 the spec layout is `[count:1][RAM-flags ptr:2]` followed by
  4-octet descriptors `[data ptr:2][config:1][type:1]`
  (`03_05_01 §4.18.9.1`).
- The sample template fits that exactly:

  ```
  19 | 07 00 | 07 1A DF 00 | 07 22 DF 00 | … | 07 50 DF 00 | 00
  ```

  - RAM-flags table at 0x0700, GO values at 0x071A–0x0750.
  - Those addresses are exactly the `AS-0700` RAM segment (82 bytes, no data).
  - Type octet `00` = 1-bit objects.
  - 104 = 1 + 2 + 25 × 4 + 1 trailing byte.
- The code's `[data_pointer][flags][00][vft]` with a 4-byte "header record"
  is off by one byte. The flags octet lands at the same offset (5 + 4·n) in
  both readings, so the output is correct by accident.
- Downstream errors:
  - `ets5-findings.md` labels AS-0700 "Application program (flash)"; it is the
    RAM segment.
  - The "8-byte slot stride" explanation is wrong (those are 16-bit RAM
    pointers).
  - The fallback `build_group_object_table` (`tables.rs:761`) emits the wrong
    layout, and `knxcfg tables` uses it for KLIC (mask 07B0) without warning.

### 7. Only one load-procedure style works

`crates/knx-prod/src/device_image.rs:257-261, 320`, `crates/knx-prod/src/app.rs:597`

- `MergedProcedure` (KLIC, AKU, BE-03S, all mask 07B0) needs the mask's
  procedure from `knx_master.xml`, with the app's `LoadProcedure MergeId`
  fragments merged in. The code takes `.first()`.
- Unknown steps become `LoadStep::Other` and are silently dropped:
  `LdCtrlRelSegment`, `LdCtrlWriteRelMem`, `LdCtrlLoadImageProp`,
  `LdCtrlWriteProp`, `LdCtrlDelay`, …
- `AppliesTo="full|par"` is ignored, so both variants would run.
- **(reproduced)** `knxcfg download` on KLIC and AKU fails with
  `Error: segment  not found`, because the System B `<AddressTable>` has no
  `CodeSegment`.
- The System B table formats are different and aren't implemented. The docs
  call mask 0705 "System B" in several places, but the ETS log
  (`CommunicationTableFormatterSystem7`) says System 7; System B is 07B0.

### 8. The download plan steps are too thin to send

`crates/knx-prod/src/device_image.rs:212-244, 296-318`

- The application's `TaskSegment` load event has to carry PEI type,
  manufacturer ID, device type and app version (`02_03_01 §3`:
  `33 02 00 43FC 01 0072 0001 01`). `DownloadStep::TaskSegment` has only an
  address.
- `AbsSegment` load events encode start and **end** address, not a size.
- Data is attached to every segment, including RAM ones (`MemType=2`, e.g.
  AS-0700 and AS-0752). ETS only allocates RAM segments; it doesn't write them.

## High: wrong data accepted silently

### 9. Bad values are silently corrupted

`crates/knx-prod/src/memory.rs:562-616`

- Parse errors become 0 (`unwrap_or(0)`); out-of-range values are truncated
  to the field width.
- **(reproduced)** `device-set 1.1.1 …_P-3 abc` writes 0;
  `…_P-3 70000` (valid range 2..240) writes 4464. The dry run still produces
  an image.
- The TUI doesn't check min/max either.
- `TypeFloat` (DPT 9), `TypeColor`, `TypeTime` and `TypePicture` encode to
  nothing, so user edits to those are dropped without a warning.

### 10. The sending group address can't be chosen

`crates/knx-prod/src/tables.rs:671-680`, `crates/knx-core/src/lib.rs:202`

- Links are sorted by (object, GA), so the lowest GA becomes the first
  association, which is what the device sends on.
- ETS lets you pick the sending GA; `GroupObjectLink` has no field for it.

### 11. Links are not checked

`crates/knxcfg/src/main.rs:975-1007`

- `link` accepts object #999, GA `0/0/0` (broadcast), and GAs that aren't in
  `groups.yaml`.
- `group-rm` leaves links pointing at the removed GA.
- **(reproduced)** `0/0/0` ended up in the Group Address Table as TSAP 1, and
  became the sending address of GO 0.
- Links to hidden objects are dropped with no warning.

### 12. Table sizes overflow silently

`crates/knx-prod/src/tables.rs:656, 674-677`: GrAT/GrOAT length, TSAP and
ASAP are cast `as u8` with no check against `MaxEntries`.

### 13. `rm` doesn't remove the device

`crates/knx-store/src/lib.rs` `save()` never deletes `devices/<ia>.yaml`.
**(reproduced)** After `rm 1.1.3` the instance comes back on the next `load`.

## Medium: design and correctness

- **Visibility logic is written three times** (`knx-form`, `tables`,
  `memory`), with duplicated `collect_module_instances` / `join_prefix`. This
  is where items 1–5 come from. Recommendation: one evaluator in `knx-prod`
  that resolves effective values (ParameterRef defaults, Assign, active union
  member) and the active set once, used by form, image and tables.
- **The read-back self-check proves nothing** (`main.rs:842-865`): it decodes
  the image it just encoded. It can't catch encoding mistakes; only a
  comparison with real ETS output can.
- **The template check points the wrong way.** `check_template`
  (179 mismatches on `00B6`) is described as "the M5 work list", but the
  template is just the legacy default image. ETS overwrites every parameter
  position anyway; e.g. `33268 = 0x81F4` is the correct enum encoding against
  a template value of `01F4`.
- **Parameter keys aren't stable across app versions.** They are app-version
  IDs (`M-0083_A-00B6-32-9642_P-3`, module instance IDs), so an application
  upgrade (`ReplacesVersions`) breaks projects. Contradicts `plan.md` §6
  ("symbolic name").
- **Bare-ID fallback is risky.** `memory.rs:507-511` and the form fall back to
  the bare parameter ID for module parameters, so one value applies to every
  instance.
- **Other knxprod gaps:**
  - ComObject/ComObjectRef `@Priority` isn't parsed (priority taken from the
    template slot).
  - ComObjectRef `Text`/`FunctionText`/`DatapointType` overrides are ignored.
  - `<Allocators>` (7 in the oracle app) aren't parsed.
  - `<Property>`-based parameters aren't handled.
  - The segment `<Mask>` element isn't used.
- **Checkbox detection is fragile.** `knx-form/src/lib.rs:491` treats a 1-bit
  enum with 2 options as off = first, on = second, by order. Real checkboxes
  are `TypeNumber UIHint="CheckBox"`; KLIC shows them as "num 0..1".
- **Recorded knxprod paths are absolute** (e.g. `/home/mw/...` in
  `test/topology.yaml`), so projects aren't portable through git.
- **Docs disagree:** `ets5-findings.md` says ASAP is "1-based" while its own
  data and `knxprod-format.md` show 0-based.
- **Template residue** in GrAT/GrOAT segments: only `len + entries` are
  overwritten; the manufacturer's default entries remain after the table end.
  Harmless given the length octet, but check against the oracle whether ETS
  writes the whole segment (SegFlags `0x80` = checksum control).

## Low / not started

- Transport is a cEMI encode/decode stub only: no USB HID frame, no
  transport-layer connection/sequencing, no `A_Memory_Write` /
  `A_PropertyValue_Write` encoding.
- No IA programming or restart handling.
- No 2-level or free group addressing.
- No DPT library from `knx_master.xml` (decision D8).
- No Data Secure support (BE-03S has `IsSecureEnabled="true"`).
- IA `x.y.0` and `15.15.255` aren't rejected.

## Suggested order of work

1. Build the single evaluator: all-matching `when`, comparison tests,
   ParameterRef values, Assign, active union member. Test it against the
   oracle `.knxproj` by comparing parameters and the visible-object set, not
   the template.
2. Re-model the 0705 GoT on the spec layout and fix the AS-0700 entry in the
   docs.
3. Fail loudly on unknown load steps, invalid or out-of-range values,
   unsupported parameter types, and table overflow.
4. Add the sending GA to `GroupObjectLink`, validate links, fix
   `rm`/`group-rm`.
5. Only then extend to MergedProcedure / System B.
