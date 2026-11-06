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
- ETS keeps values and refs of objects and parameters that are currently hidden.
- A device can have no parameter and object refs at all (`in16-tests` 1.1.1).

## ETS 5.7 differences

- `ComObjectInstanceRef` has `@Links` (`GA-48 GA-23`) instead of `Connectors`.
  The ids are the end of `GroupAddress/@Id` (`P-0191-0_GA-48`). The first one is
  taken as the sending address, because the ref has no other attribute that could
  hold it. **This is not confirmed.** The order of the ids says nothing, they are
  not addresses. All 17 multi-link refs seen are also in ascending address order,
  so "sending first" and "sorted by address" cannot be told apart yet.
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

## Not imported

Reported by `knxcfg import`; the model has no place for these yet.

- Set per object in ETS and **changes the download**: `ReadFlag`, `WriteFlag`,
  `CommunicationFlag`, `TransmitFlag`, `UpdateFlag`, `ReadOnInitFlag`, `Priority`.
  A value that differs from the product gets a warning for that object.
- Needed for a faithful export (M3): `Buildings`/`Locations` and functions (D7), group range
  names, group address style, descriptions, serial numbers, object `Text`,
  `FunctionText` and `DatapointType`, and the values of inactive refs of a
  parameter with several refs.
