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
  index into the (sorted) GrAT; ASAP = the knxprod `ComObject/@Number`
  (**0-based** in this application: `00` = GO0).
- `03 01 00 02 06 03 17` = `0/0/10`→GO0, `0/0/11`→GO6, `0/0/12`→GO23 (0x17).

### Group Object Table (GoT)

- Layout per `03_05_01 §4.18.9.1` (mask 0701h/0705h, 16-bit pointers):
  `[count:1][RAM-flags table pointer:2]`, then one 4-octet descriptor per
  object in ASAP order: `[data pointer:2][config:1][type:1]`.
- The oracle bytes read as `19 | 07 00 | 07 1A 4F 00 | 07 22 DB 00 | …`:
  25 objects, RAM-flags table at `0x0700`, GO0 value at `0x071A` with config
  `4F`, type `00` (1 bit), GO1 value at `0x0722`, config `DB`, …
  104 octets = 1 + 2 + 25 × 4 + 1 trailing octet.
- The config octet uses the `03_05_01 §4.18.4.1` RT2 bit layout
  (0x80 update, 0x40 transmit, 0x20 segment-selector, 0x10 write, 0x08 read,
  0x04 comm, 0x03 priority). The knxprod template carries `0xDF` (all flags
  on, segment selector 0, low priority) in every GO slot; ETS overwrites it
  with the realized flags (`4f`/`db`/`d3`/`4b`).

## Memory map

| Segment | Address | Size | Purpose |
|---------|---------|------|---------|
| AS-4000 | 0x4000  | 513  | AddressTable (GrAT) |
| AS-4201 | 0x4201  | 511  | AssociationTable (GrOAT) |
| AS-4400 | 0x4400  | 104  | ComObjectTable (GoT) |
| AS-4468 | 0x4468  | 168  | Application program (EEPROM params) |
| AS-0700 | 0x0700  | 82   | RAM (`MemType=2`, `Access=0`): RAM-flags table and group object values. Allocated by the load procedure, not written |

The GrAT/GrOAT/GoT download via `A_Memory_Write` to the segment addresses; the
load state is driven by `A_PropertyValue_Write` to PropertyId 5
(LoadStateControl): 01=Start, 03=RelativeAllocation, 02=Complete, 04=Unload.

## Data pointers — template-fixed

The data pointers (`0x071A`, `0x0722`, …) are 16-bit addresses inside the
application's RAM segment `AS-0700` (`0x0700`–`0x0751`); the RAM-flags table
starts at `0x0700`. They are **byte-identical** between the knxprod template
and the ETS download, so they are not computed: the GoT is produced by
overlaying the config (and type) octet of each descriptor onto the template.
(An earlier reading of the GoT as `[data_pointer:1][flags][00][vft]` records
behind a 4-octet "header record" was off by one octet; it put the flags at
the same offsets by coincidence. The "8-byte slot stride" seen in that reading
is simply the spacing of 16-bit RAM addresses.)

ETS names the formatter `CommunicationTableFormatterSystem7`: mask 0705h is
**System 7** (BIM M112). System B is mask 07B0h, which uses property-based
tables (`03_05_01 §4.16.8 / §4.17.5 / §4.18.6`), see `crates/knx-prod/src/tables.rs`.

## Module GO ASAP enumeration — mostly done, one caveat

Mapping the knxprod `ComObject` set to the flat ASAP `0..N-1` (and thus the GoT
record index) is `ASAP = BaseNumber-argument value + Number`, where the
`BaseNumber` argument (`BaseObj`, `BaseObjGroupt`, `ObjNumberBase`) carries the
per-instance stride base and its `Allocates` is the per-instance stride.
`group_objects()` implements this and was verified against the oracle:

- **Flags layout confirmed**: the device `Mode` GO (transmit/read/comm enabled,
  write disabled) → flag bits `0x4C` (transmit 0x40 + read 0x08 + comm 0x04),
  plus template priority low → `0x4F`; unlinked, so communication is cleared
  → `0x4B`.
- **Logic ASAP confirmed**: `#23` = `ObjNumberBase 9 + Number 14` (the 4th
  Logic output `Ausgangslogik 1`) — matching the GrOAT `GO 23 ↔ 0/0/12`.

Caveats:
- The GoT has 25 descriptors (ASAP 0–24); descriptor 24 is the device GO
  "In Betrieb", not padding. Confirmed against ETS: the
  "Inputs grouped" mode (`MD-1`) and the single-input mode (`MD-2`, "Eingang
  A"/"Eingang B") are mutually exclusive — only the active mode's objects
  appear in the Kommunikationsobjekte pane.
- Device-level GO "In Betrieb" (`Mode`, Number 24) → ASAP **24**, confirming
  `ASAP = Number` for device GOs and `ASAP = base + Number` for module GOs
  (verified: Eingang A "Zustand senden"=0, "Sperrobjekt"=5; Eingang B
  "Zustand senden"=6).
