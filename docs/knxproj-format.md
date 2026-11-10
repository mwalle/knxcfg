# `.knxproj` format

Observed in two ETS 5.0 exports (namespace `project/12`, 5.0.864 and 5.0.1146)
and one ETS 5.7 archive (`project/20`, 5.7.1093, `.knxprojarchive`, same content
as a `.knxproj`), all in `refs/knxproj`. The format is ETS-proprietary and not in
the KNX specs. Parsing is in `crates/knx-proj`.

## Archive

```
P-XXXX/project.xml      ProjectInformation: Name, GroupAddressStyle
P-XXXX/0.xml            Installation: Topology, Buildings, GroupAddresses
P-XXXX.signature
M-XXXX/Catalog.xml      product data of every manufacturer the project has used
M-XXXX/Hardware.xml
M-XXXX/M-XXXX_A-*.xml
M-XXXX.signature
knx_master.xml
```

- The product data is the same layout as a knxprod, but may cover several
  manufacturers and applications no device uses any more (`in16-tests` carries
  `M-0004` without any device of it). A knxprod holds one manufacturer, so the
  import writes `vendor/<file>-<manufacturer>.knxprod` per manufacturer it needs.
- Files start with a byte order mark.

## Installation

```
Topology/Area[@Address]/Line[@Address]/DeviceInstance[@Address @Hardware2ProgramRefId]
  ParameterInstanceRefs/ParameterInstanceRef[@RefId @Value]
  ComObjectInstanceRefs/ComObjectInstanceRef[@RefId @IsActive @Priority @DatapointType ...]
    Connectors/Send[@GroupAddressRefId]
GroupAddresses/GroupRanges/GroupRange/GroupRange/GroupAddress[@Id @Address @Name @DatapointType]
```

- `GroupAddress/@Address` is the raw 16-bit value. `@DatapointType` is a
  space-separated list, seen as `DPST-1-1 DPST-1-1`.
- `ParameterInstanceRef/@RefId` is a `ParameterRef/@Id`, **not** a parameter: a
  parameter with several refs can hold a different value per ref
  (`P-20008_R-20018` = 100, `P-20008_R-123` = 10). The value that counts is the
  one of the active ref; the model stores that one.
- `ComObjectInstanceRef/@RefId` is a `ComObjectRef/@Id`. An object can be listed
  through several refs (`O-15_R-10125` inactive, `O-15_R-10127` active).
- `@IsActive` is ETS's visibility result, so it is the oracle for the evaluator.
- `@Priority` is written even if the product states none (then `Low`).
- ETS 5.7 writes the flags (`@ReadFlag`, `@WriteFlag`, `@CommunicationFlag`,
  `@TransmitFlag`, `@UpdateFlag`, `@ReadOnInitFlag`, values `Enabled`/`Disabled`)
  and `@Priority` (`Low`/`High`/`Alert`) only where the user changed them in
  the object's properties (knx-sim9 export). The import keeps the values that
  differ from the product as `objects` in `devices/<ia>.yaml`, and the download
  applies them; the device state equals ETS's (knx-sim, System 7 and System B).
- ETS keeps values and refs of objects and parameters that are currently hidden.
- A device can have no parameter and object refs at all (`in16-tests` 1.1.1).

## ETS 5.7 differences

- `ComObjectInstanceRef` has `@Links` (`GA-48 GA-23`) instead of `Connectors`.
  The ids are the end of `GroupAddress/@Id` (`P-0191-0_GA-48`). **The first one is
  the sending address** (confirmed with ETS 5.7.4, 2026-11-10, knx-sim step 6):
  a GO linked to 0/0/10 (sending) and 0/0/11 exported as `Links="GA-1 GA-2"`;
  after "Bearbeiten → Sendend setzen" on 0/0/11 the same export has
  `Links="GA-2 GA-1"`, and nothing else in the file changes. ETS's download then
  puts the association with 0/0/11 first, as knxcfg does.
- Only values that differ from the default are stored.
- No `@IsActive` or `@Priority`, and refs of hidden objects stay listed without
  links. The state of an object can only be read one way: **having links or being
  listed in `GroupObjectTree//Node/@GroupObjectInstances` means active**. Not all
  active objects are in the tree (linked objects were missing from it). The import
  checks this direction only, so it does not notice an object the evaluator
  activates wrongly.
- Parameter refs keep the application prefix, object refs drop it:
  `M-0083_A-00B6-32-9642_P-5001_R-5001`, `O-2_R-31`.
- Module refs name the instance:
  `M-0083_A-00B6-32-9642_MD-2_M-2_MI-1_P-24_R-2` and `MD-2_M-2_MI-1_O-3-0_R-128`
  are `P-24_R-2` / `O-3-0_R-128` of definition `MD-2`, instance `MD-2_M-2`. Only
  `MI-1` has been seen, so others are not resolved; nested `SM-` neither.
- `DeviceInstance/ModuleInstances/ModuleInstance[@RefId @RepeatIndex]/Arguments/
  Argument[@RefId @Value]` repeats the module arguments. They equal the product's
  `TextArg`/`NumericArg`, and the import compares them because they set the object
  numbers. `@RepeatIndex` (`21x1`) is not understood.
- `Topology/UnassignedDevices/DeviceInstance` holds devices on no line, without
  `@Address`. These are reported and skipped. A device without an application has a
  `Hardware2ProgramRefId` that stops at `..._HP`.
- `Installation/Locations` replaces `Buildings`.

## Not seen in a real file

Supported from general knowledge, with no sample to check it against. A `RefId`
that does not lead to an element of the application is reported as a warning, so
a wrong assumption loses data visibly rather than writing it to the wrong place.

- `Connectors/Receive` next to `Send`.
- `Line/Segment/DeviceInstance`.
- Password-protected projects (`P-XXXX.zip` instead of `P-XXXX/`) are refused.

## Group addresses

- `GroupAddress/@Unfiltered="true"`: "Weiterleiten (nicht filtern)" in ETS; the
  address passes every coupler (imported as `unfiltered`, used by
  `Project::coupler_filter`).

## Kept for the export

Besides topology, group addresses, parameters, links and object settings, the
import keeps what an export (M3) needs: the group address style and project
comment (`knx.yaml`), line media (`MediumTypeRefId`: MT-0 TP, MT-1 PL, MT-2 RF,
MT-5 IP) and descriptions, the device's `ProductRefId`, description, comment
and serial number, group address descriptions and comments, the group ranges
(`ranges.yaml`) and the building structure (`Locations/Space` with its
`DeviceInstanceRef`s, `locations.yaml`). ETS 5.7 writes `Locations`; it has no
trades or functions.

An object's `Text`, `FunctionText` and `DatapointType` are kept only where they
differ from the product: ETS writes the text it shows (project language,
placeholders filled in, e.g. `Eingang A: ` for `Input {{ChNo}}: {{0}}`), so a
text that matches the product's template in one of its languages counts as the
product's.

## Not imported

- The values of refs that are not active, for a parameter with several refs.
- What ETS keeps for itself: download state (`*Loaded`, `LoadedImage`,
  `CheckSums`), security, timestamps, `Puid`s.

## Export

`knxcfg export <file>` (`knx_proj::export`) writes what ETS 5.7.4 imports
(checked in the VM, 2026-11-10):

- `P-XXXX/project.xml` and `P-XXXX/0.xml` from the model, like ETS writes them:
  only stored parameter values (`ParameterInstanceRef` on the parameter's
  active ref; module refs as `<app>_MD-n_M-n_MI-1_P-n_R-n`), only objects with
  links or own settings (`ComObjectInstanceRef`, module objects as
  `MD-n_M-n_MI-1_O-n_R-n` without the application prefix, `Links` with the
  sending address first), `Locations`, and the group ranges. ETS computes the
  module instances and their arguments itself, so `ModuleInstances` is left out.
  `CreatedBy="knxcfg"` is accepted.
- ETS fails on a project without the backbone (area 0, line 0) or without an
  area's main line (line 0): "Die Sequenz enthält keine Elemente" when the
  topology is shown. The export adds them where the model has none, and gives
  group addresses outside every stored range a main and middle group.
- The product data is copied from the devices' knxprods unchanged: the
  application programs in use, `Catalog.xml` and `Hardware.xml` (merged by
  element id when one manufacturer's devices come from several knxprods), other
  files, and the largest `knx_master.xml`. ETS accepts the manufacturer's files
  as they are; it trims them in its own exports.
- Every directory gets `<dir>.signature`: base64 of an RSA PKCS#1 v1.5 SHA-1
  signature over the SHA-1 of the manifest `name:base64(sha1(content))` of the
  directory's files (names relative to the directory with `\` separators,
  sorted, joined with `,`). ETS rejects a project without a valid signature
  ("Projektdatei hat keine gültige Signatur"), also with an older schema. The
  key is not part of knxcfg: `--signing-key <pem>` or
  `~/.config/knxcfg/ets-signing-key.pem`; without one the file is unsigned.
- Alternatively `--signing-dll <Knx.Ets.XmlSigning.dll>` signs with ETS's own
  code: a helper (`crates/knxcfg/src/ets_signer/EtsSign.cs`, compiled once into
  the cache directory) calls the assembly's internal
  `XmlSigning.SignDirectory(dir, false, { ".chw" })` by reflection; it writes
  `<dir>.signature` (with a UTF-8 BOM) next to the directory. On Windows with
  .NET Framework this gives exactly ETS's signatures (checked in the VM: both
  directories of the knx-sim9 export, byte for byte apart from the BOM). On
  Linux the helper runs under Mono with `libshlwapi.so`
  (`ets_signer/shlwapi.c`) for the assembly's `PathRelativePathTo` call, but
  Mono 6.12 cannot run the obfuscated assemblies: ETS 5.7.4's fails with
  `index < meta->heap_strings.size` while loading, the 5.6 converter build's
  `SignDirectory` with "Invalid IL code". Debian has no Microsoft .NET runtime;
  whether that runs it is not tried.

Checked: ETS's download of an exported project (knx-sim's devices with flags,
priorities and links; and a project made with `knxcfg add/link/device-set/
object-set` from two knxprods, with module parameters and a building) leaves
the same device state in knx-sim as ETS's download of the original project and
as knxcfg's own download. Importing the export again gives the same model,
plus the added backbone, main lines and group ranges.
