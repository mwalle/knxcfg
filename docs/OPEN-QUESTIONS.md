# GoT flag byte / record order (M5) — RESOLVED

## Resolution

The GoT is produced by **overlaying the config octets onto the knxprod
`<ComObjectTable>` template**. The template (`AS-4400`, 104 bytes) has the
`03_05_01 §4.18.9.1` layout `[count:1][RAM-flags pointer:2]` followed by
4-octet descriptors `[data pointer:2][config:1][type:1]` in ASAP order.

ETS reads the count, then for each descriptor either

1. writes the **resolved `ComObjectRef` config** (and type) (active object),
   or
2. keeps the **template's config with the communication bit cleared**
   (inactive object).

## Template (from knxprod `AS-4400`)

```
19 | 07 00             ← 25 objects, RAM-flags table at 0x0700
07 1a df 00            ← ASAP 0:  value at 0x071A, config DF, type 0 (1 bit)
07 22 df 00            ← ASAP 1
...
07 4f df 00            ← ASAP 23
07 50 df 00            ← ASAP 24 (Mode/"In Betrieb")
00                     ← trailing octet
```

## Flags octet (RT2 config octet, `03_05_01 §4.18.4.1`)

```
bit 7 (0x80) update           bit 3 (0x08) read
bit 6 (0x40) transmit         bit 2 (0x04) communication
bit 5 (0x20) segment selector bit 1..0     priority (from template slot)
bit 4 (0x10) write
```

Bit 5 is the **segment selector** (0 = segment 00h), *not* the System-B “value
read on init” flag — that feature is unsupported in this memory-mapped layout
and is greyed out in ETS. The template's slot flags `0xDF` = “all flags on,
segment selector 0, low priority” (a manufacturer/tool default, not a value ETS
computes). ETS writes:

| object state | flags octet |
|---|---|
| visible, linked, comm on | resolved flags + template low priority |
| visible, unlinked | resolved flags + low priority, comm cleared |
| visible, linked, comm off (user) | resolved flags + low priority, comm cleared |
| hidden/inactive | template slot flags with comm cleared = `0xDB` |

Verified against the experiment (`/tmp/experiment.txt`, current state GO0 comm
cleared):

```
ASAP 0  (Zustand senden, comm cleared): 4B
ASAP 5  (Sperrobjekt,    unlinked):      D3
ASAP 6  (Zustand senden, linked):        4F
ASAP 14 (Logik 1 Ausgang, unlinked):     4B
ASAP 23 (Logik 4 Ausgang, linked):       4F
ASAP 24 (In Betrieb,      unlinked):     4B
all others (hidden):                     DB
```

`crates/knx-prod/tests/oracle.rs` reproduces the linked/unlinked cases with
Logic 4 enabled and cyclic "Operation" on.

## Visibility

A GO is **visible** (and therefore gets its resolved flags) only when

- its `<Static>` defines **no `<ComObjectRefs>`** (every base object is always
  visible), or
- its base `ComObject` is selected by a `ComObjectRefRef` on the active
  `choose`/`when` path of the `Dynamic` tree.

Objects not selected are hidden and keep the template slot flags with comm
cleared (`0xDB`).

The ETS oracle project (`/tmp/test.knxproj`) shows GOs `0, 5, 6, 14, 23, 24`
visible; the remaining 19 are hidden. This is parameter-dependent (per-channel
function, per-logic enable, cyclic “Operation”), so reproducing the exact set
needs the project's parameter values (M2 `.knxproj` import). With knxprod
defaults only GO 0 and GO 6 are active (all matching `when` branches are
evaluated, see `crates/knx-prod/src/eval.rs`).

## Code changes landed

- `tables::go_descriptor_high` — RT2 config-octet flag bits (bit 5 = segment
  selector = 0; value-read-on-init is not encoded), no priority.
- `tables::visible_group_object_flags` — resolved flags + template slot
  priority, comm cleared when unlinked.
- `tables::group_object_table_entry_count` — reads the count octet.
- `tables::overlay_group_object_table` — keyed by ASAP over the
  `§4.18.9.1` descriptors; writes config + type for active objects and
  `template config & !0x04` for inactive ones.
- `tables::active_group_objects` — only emits visible objects (module + device
  level), honoring the no-`ComObjectRefs` fallback.
- `device_image::build_device_image` — passes final flags bytes keyed by ASAP,
  computing comm-clear from the project links.

Remaining (already tracked in M2): project-level flag/priority overrides and
the exact visible set need `.knxproj` import.
