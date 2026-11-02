# knxprod format — observed structure

> These notes document the structure of the knxprod files in
> `refs/knxprod/` (MDT, KLIC-MITT). knxprod is a KNX-standardized product
> format; the normative schema ships as XSD with the KNX Manufacturer Tool
> (not in `refs/spec`). Everything below is observed from sample files and
> will be cross-checked as the parser grows.
>
> Namespace observed: `http://knx.org/xml/project/20` (ETS5-era products,
> `MinEtsVersion="5.0"`).

## Container

A knxprod file is a ZIP archive:

```
knx_master.xml                    # MasterData: DPTs, languages, medium types, …
M-XXXX.signature                  # signature of the manufacturer folder
M-XXXX/Catalog.xml                # catalog sections + catalog items
M-XXXX/Hardware.xml               # hardware definitions (mask versions, …)
M-XXXX/M-XXXX_A-XXXX-XX-XXXX.xml  # one file per application program
M-XXXX/Baggages/…                 # images etc. (optional)
```

`M-XXXX` is the manufacturer id (`M-0083` = MDT, `M-0071` = KLIC-MITT).

## knx_master.xml

`<MasterData Id="MD-1" Version="226" Signature="…">` containing, among
others:

- `<DatapointTypes>` → `<DatapointType Id="DPT-1" Number="1" Name="1.xxx"
  Text="1-bit" SizeInBit="1" PDT="PDT-50">` with nested
  `<DatapointSubtypes>` → `<DatapointSubtype Id="DPST-1-1" Number="1"
  Name="DPT_Switch" Text="switch" Default="true">` + `<Format>` (bit
  clear/set labels for enums).

## Catalog.xml

```
<ManufacturerData>
  <Manufacturer RefId="M-0083">
    <Catalog>
      <CatalogSection Id=… Name=… Number=… VisibleDescription=… DefaultLanguage=…>
        <CatalogItem Id=… Name=… Number=… VisibleDescription=…
                     ProductRefId=… Hardware2ProgramRefId=… DefaultLanguage=… />
```

- `CatalogItem` links a product (hardware) to its application program via
  `Hardware2ProgramRefId` (e.g. `M-0083_H-180-0_HP-00B4-32-3919`).
- The id pattern encodes hardware (`H-180-0`) and application program
  (`A-00B4-32-3919`, i.e. application number `0x00B4`, version `0x32`).

## Application program file

`<ApplicationProgram Id="M-0083_A-00B4-32-3919" ApplicationNumber="180"
ApplicationVersion="50" ProgramType="ApplicationProgram"
MaskVersion="MV-0705" Name=… LoadProcedureStyle="ProductProcedure"
PeiType="1" DefaultLanguage="en-US" DynamicTableManagement="false"
Linkable="false" MinEtsVersion="5.0" …>`

Contents (to be parsed in M1):

- `<Static>` — the static interface:
  - `<Code>` → `<AbsoluteSegment Id=… Size=… Address=…>` → `<Data>` (base64,
    the application code) and `<RelativeSegment>` (parameters).
  - parameter blocks/types (dynamic UI rules live here),
  - `<ComObjectTable>` / `<ComObjectRefs>`,
  - `<AddressTable>`, `<AssociationTable>`,
  - `<LoadProcedures>`.
- `<Dynamic>` — dynamic table management (rare for these devices).

## Notes for M1

- Parameter types carry visibility/enablement `compute` expressions that
  reference other parameters by refId — the source of the dynamic UI
  (see `docs/plan.md` D3 / `crates/knx-form`). In the samples this is
  expressed as `choose` → `when test=…` → `ParameterBlock` (not a literal
  `compute` element).
- `LoadProcedureStyle="ProductProcedure"` or `"MergedProcedure"` means the
  memory image is built from the parameter/GO/address tables + load
  procedure, which is what M5's download builder must produce.

## Parsed in M1 (implemented)

- `Static`: `ParameterTypes` (`TypeNumber`, `TypeRestriction`+`Enumeration`,
  `TypeText`, `TypeIPAddress`), `Parameters` (interleaved `Parameter` +
  `Union` via `$value`/enum), `ParameterRefs`, `ComObjectTable` or
  `ComObjects` (`ComObject`).
- `ModuleDefs` → `ModuleDef` → `Static` (per-channel parameter/GO templates).
- `Hardware.xml` → `Hardware2Program` → `ApplicationProgramRef` (h2p → app).
- `<Languages>` → `<Language Identifier>` → `<TranslationUnit>` →
  `<TranslationElement RefId>` → `<Translation AttributeName Text>`; used for
  localized labels (`AttributeName="Text"`).
- Group objects may live at device level (`ComObjectTable`) or in module
  definitions (`ComObjects`); `knxcfg probe` merges both.

Form-engine rules (`crates/knx-form`):

- Top-level `ParameterBlock`/`Channel` in `Dynamic` = a **page** (ETS pane);
  nested blocks/separators = section titles.
- A `Module` page/section is titled by the instance `Name` attribute when
  present; otherwise by the `ModuleDef`'s top-level `ParameterBlock` `Text`
  template (`{{ChNo}}` = the instance's `ChNo` argument from `<Arguments>`,
  `{{0}}` = the `TextParameterRefId` parameter). E.g. MDT's `ModuleDefLogic`
  has `Text="Logic {{ChNo}}: {{0}}"` and numeric `ChNo` 1..4 → "Logic 1..4".
  That top-level title block is unwrapped (not rendered as a section).
- `Access="None"` parameters are hidden; `Access="ReadOnly"` fields are
  shown but not editable.
- Labels resolve: translation → `@Text` → `@Name` (this removes the raw
  `OM_`-style internal names from the UI).

Not yet parsed (download builder, M2/M5):

- `AddressTable` / `AssociationTable` / `LoadProcedures` contents.
- `Code` segments (`AbsoluteSegment`/`RelativeSegment` base64 data).
- `Assign` value computation (M2) and full module memory-offset
  instantiation (M5). The M1.5 form walk treats `Assign` as a no-op.
