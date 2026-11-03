# M5 — ETS5 oracle findings (byte-format reverse-engineering)

> Source: live TP1 bus capture **and the ETS5 verbose log** (`/tmp/ETS5.log`,
> `CommunicationTableFormatterSystem7`) of a full download of a **BDT/AB
> "Universal Interface 2-fold"** (`BE-02001.02`, knxprod
> `M-0083_A-00B6-32-9642`), mask `MV-0705`, manufacturer `M-0083`.
> Configured links: `0/0/10`→GO0, `0/0/11`→GO6, `0/0/12`→GO23.

## Definitive table formats (ETS's own `CommunicationTableFormatterSystem7`)

```
Address table     at $4000: 04 00 00 00 0A 00 0B 00 0C
Association table at $4201: 03 01 00 02 06 03 17
GroupObject table at $4400: 19 07 00 07 1A 4F 00 07 22 DB 00 07 ... (26 × 4 octets)
```

### Group Address Table (GrAT)

- `[length:1][IA:2][GA:2 × N]` — length is **1 octet** and counts the IA plus
  the GAs (`04` = 1 IA + 3 GAs). IA = `00 00` (0.0.0). GAs are 16-bit
  big-endian, ascending.

### Association Table (GrOAT, format 0)

- `[length:1][TSAP:1][ASAP:1] × N` — length is **1 octet**. TSAP = 1-based
  index into the (sorted) GrAT; ASAP = group object number (1-based).
- `03 01 00 02 06 03 17` = `0/0/10`→GO0, `0/0/11`→GO6, `0/0/12`→GO23 (0x17).

### Group Object Table (GoT)

- **No length prefix.** Each GO is **4 octets**: `[word0][flags][00][vft]`.
- `flags` uses the `03_05_01 Table 87` bit layout (0x80 update, 0x40 transmit,
  0x20 read-init, 0x10 write, 0x08 read, 0x04 comm, 0x03 priority). The
  knxprod default templates carry a placeholder `0xDF` that ETS overwrites
  with realized flags (`4f`/`db`/`d3`/`4b`).

## Memory map

| Segment | Address | Size | Purpose |
|---------|---------|------|---------|
| AS-4000 | 0x4000  | 513  | AddressTable (GrAT) |
| AS-4201 | 0x4201  | 511  | AssociationTable (GrOAT) |
| AS-4400 | 0x4400  | 104  | ComObjectTable (GoT) |
| AS-4468 | 0x4468  | 168  | Application program (EEPROM params) |
| AS-0700 | 0x0700  | 82   | Application program (flash) |

The GrAT/GrOAT/GoT download via `A_Memory_Write` to the segment addresses; the
load state is driven by `A_PropertyValue_Write` to PropertyId 5
(LoadStateControl): 01=Start, 03=RelativeAllocation, 02=Complete, 04=Unload.

## The `word0` octet — SOLVED: it is the **Data Pointer**

`word0` is the 1-octet **Data Pointer** of the Group Object Descriptor
(`03_05_01 §4.18.3.1.2`): it points to the MSB of the Group Object's value in
its data segment (RAM/EEPROM). The segment is selected by the **Segment
Selector** bit (bit 5) of the Config octet: `0` → segment `00h`, `1` → segment
`100h`. So `word0` is the GO's **value address/offset** in its data segment.

**Crucially, comparing the knxprod template against ETS's download shows the
`data_pointer` is byte-identical in every record — ETS only rewrites the
flags octet (and the end marker).** So the data pointer is **not computed at
all**; it is baked into the knxprod `ComObjectTable` segment template. The GoT
is produced by overlaying flags onto the template keyed by ASAP (record
index), exactly like `build_parameter_image` overlays parameter values onto the
`<Code>` templates.

This explains the observed strides: 1-bit objects have adjacent pointers
(`0x19, 0x1A`), and 4-byte objects sit on 8-byte slots (`0x22, 0x2A, 0x32, …`),
reflecting the device's value-area allocation.

Full System-B (mask 0705) memory-mapped descriptor is therefore **4 octets**:
```
[Data Pointer:1][Config/Flags:1][Type:1][? or padding:1]
```
with the flags octet matching RT1/RT2 (`0x80 update, 0x40 transmit, 0x20
segment-selector, 0x10 write, 0x08 read, 0x04 comm, 0x03 priority`) and the
type octet = the Value Field Type (`0..6` = 1..7 bits, `7` = 1 octet, `8` = 2
octets, …). `tables.rs::overlay_group_object_table(template, flags_by_record)`
implements this.

## Module GO ASAP enumeration — mostly done, one caveat

Mapping the knxprod `ComObject` set to the flat ASAP `0..N-1` (and thus the GoT
record index) is `ASAP = BaseNumber-argument value + Number`, where the
`BaseNumber` argument (`BaseObj`, `BaseObjGroupt`, `ObjNumberBase`) carries the
per-instance stride base and its `Allocates` is the per-instance stride.
`group_objects()` implements this and was verified against the oracle:

- **Flags layout confirmed**: the device `Mode` GO (transmit/read/comm enabled,
  write disabled) → `0x4C` (`0100 1100` = transmit 0x40 + read 0x08 + comm
  0x04), matching the RT2 config octet exactly.
- **Logic ASAP confirmed**: `#23` = `ObjNumberBase 9 + Number 14` (the 4th
  Logic output `Ausgangslogik 1`) — matching the GrOAT `GO 23 ↔ 0/0/12`.

Caveats:
- The GoT has 26 records but the device has only 25 group objects
  (ASAP 0–24: channels A/B 0–11, Logic 12–23, "In Betrieb" 24); record 25 is
  unused padding (the template's last record `50 db 00 00`). Confirmed against
  ETS: the "Inputs grouped" mode (`MD-1`) and the single-input mode (`MD-2`,
  "Eingang A"/"Eingang B") are mutually exclusive — only the active mode's
  objects appear in the Kommunikationsobjekte pane.
- Device-level GO "In Betrieb" (`Mode`, Number 24) → ASAP **24**, confirming
  `ASAP = Number` for device GOs and `ASAP = base + Number` for module GOs
  (verified: Eingang A "Zustand senden"=0, "Sperrobjekt"=5; Eingang B
  "Zustand senden"=6).
