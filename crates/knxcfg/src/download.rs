//! Convert a knxprod download plan into cEMI management frames.
//!
//! This is the glue between `knx-prod`'s `download_plan` (the abstract
//! `<LoadProcedure>` sequence with assembled segment bytes) and
//! `knx-transport`'s wire encoding (cEMI `L_Data` frames carrying the `A_*`
//! management APDUs). It lives in the CLI crate because the layers are strict:
//! `knx-prod` (commissioning) and `knx-transport` (protocol) both stay below
//! the orchestrator — see `docs/plan.md` §4.
//!
//! [`plan_to_frames`] gives the request side of the conversation only. Steps
//! whose frames depend on a device response cannot be pre-encoded and are
//! reported as errors: `LdCtrlWriteRelMem` needs the object's
//! `PID_TABLE_REFERENCE`, and steps that name an interface object by type
//! (couplers) need its index. [`execute`] runs a plan over a live connection
//! and checks the device's answers.

use anyhow::{bail, Context, Result};
use knx_prod::device_image::filter_table_writes;
use knx_prod::{DownloadStep, Object};
use knx_transport::apdu::{self, apci};
use knx_transport::cemi::{self, tpci};
use knx_transport::mgmt::{self, Connection};
use knx_transport::{CemiFrame, Transport};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Data octets per `A_Memory_Write` in a standard frame: 15 TPDU octets
/// after the TPCI octet, minus APCI and address (`03_03_07 §3.5.4`).
const MEMORY_WRITE_CHUNK: usize = 12;

/// Data octets per `A_FilterTable_Write` in a standard frame: one octet less
/// than `A_Memory_Write`, for the separate count (`03_05_02 §3.41.2`: 11).
const FILTER_TABLE_WRITE_CHUNK: usize = 11;

/// `PID_LOAD_STATE_CONTROL` writes carry one 10-octet `PDT_CONTROL` element
/// (`03_05_02 §3.31.3`).
const LOAD_EVENT_LEN: usize = 10;

/// Encode a download plan as the sequence of cEMI frames to send.
///
/// `dst` is the device's Individual Address. The whole download runs over a
/// single Transport Layer connection: `T_Connect` → `T_Data_Connected` (with
/// incrementing sequence numbers) → `T_Disconnect`.
pub fn plan_to_frames(plan: &[DownloadStep], dst: u16) -> Result<Vec<CemiFrame>> {
    let mut frames = Vec::new();
    let mut seq: u8 = 0;
    let mut data = |frames: &mut Vec<CemiFrame>, apdu: &[u8]| {
        frames.push(cemi::l_data_req(tpci::T_DATA_CONNECTED | tpci::seq(seq), 0, dst, apdu));
        seq = (seq + 1) & 0x0F;
    };

    for (i, step) in plan.iter().enumerate() {
        let index = |object: &Object| -> Result<u8> {
            match object {
                Object::Index(idx) => Ok(*idx),
                Object::Type(t) => bail!(
                    "step {}: the load procedure names the interface object by type ({t}); its \
                     index is read from the device, so this can only be encoded during a live \
                     download",
                    i + 1
                ),
            }
        };
        match step {
            DownloadStep::Connect => {
                frames.push(cemi::l_data_control(tpci::T_CONNECT, 0, dst));
            }
            DownloadStep::Disconnect => {
                frames.push(cemi::l_data_control(tpci::T_DISCONNECT, 0, dst));
            }
            DownloadStep::Restart => data(&mut frames, &apdu::restart()),
            DownloadStep::CompareProp { obj_idx, prop_id, inline_data } => {
                // Verify = read the property; the caller compares the
                // response against `inline_data`.
                let _ = inline_data;
                data(&mut frames, &apdu::property_value_read(*obj_idx, *prop_id, 1, 1));
            }
            DownloadStep::ReadProp { obj_idx, prop_id } | DownloadStep::CompareLoadedProp { obj_idx, prop_id } => {
                data(&mut frames, &apdu::property_value_read(*obj_idx, *prop_id, 1, 1));
            }
            DownloadStep::WriteProp { object, prop_id, data: d, .. } => {
                let idx = index(object)?;
                data(&mut frames, &apdu::property_value_write(idx, *prop_id, 1, 1, d));
            }
            DownloadStep::Unload { object } => {
                let idx = index(object)?;
                data(&mut frames, &load_event(idx, &[0x04]))
            }
            DownloadStep::Load { object } => {
                let idx = index(object)?;
                data(&mut frames, &load_event(idx, &[0x01]))
            }
            DownloadStep::LoadCompleted { object } => {
                let idx = index(object)?;
                data(&mut frames, &load_event(idx, &[0x02]))
            }
            DownloadStep::WriteMem { address, data: bytes, .. } => {
                if *address as usize + bytes.len() > 0x1_0000 {
                    bail!("step {}: write at {address:#x} goes above 64 KiB", i + 1);
                }
                for (n, chunk) in bytes.chunks(MEMORY_WRITE_CHUNK).enumerate() {
                    let offset = *address as usize + n * MEMORY_WRITE_CHUNK;
                    data(&mut frames, &apdu::memory_write(offset as u16, chunk));
                }
            }
            DownloadStep::WriteFilterTable { address, data: bytes, cleared, .. } => {
                if *address as usize + bytes.len() > 0x1_0000 {
                    bail!("step {}: filter table at {address:#x} goes above 64 KiB", i + 1);
                }
                data(&mut frames, &apdu::filter_table_open());
                for (offset, chunk) in filter_table_writes(bytes, *cleared, FILTER_TABLE_WRITE_CHUNK) {
                    data(&mut frames, &apdu::filter_table_write((*address as usize + offset) as u16, chunk));
                }
            }
            DownloadStep::ClearFilterTable { object } => {
                let idx = index(object)?;
                data(
                    &mut frames,
                    &apdu::function_property_command(
                        idx,
                        apci::PID_ROUTETABLE_CONTROL,
                        &[0x00, apci::SRVID_CLEAR_FILTERTABLE],
                    ),
                );
            }
            DownloadStep::AbsSegment {
                lsm_idx,
                seg_type,
                address,
                size,
                access,
                mem_type,
                seg_flags,
                data: bytes,
            } => {
                // 1. Allocate the segment: event 03h, segment type, start
                //    address, length, access, memory type, memory attributes
                //    (`03_05_02 §3.31.3.4`, AllocAbsDataSeg/AllocAbsStackSeg).
                let [a0, a1] = (*address as u16).to_be_bytes();
                let [l0, l1] = (*size as u16).to_be_bytes();
                let event = [0x03, *seg_type, a0, a1, l0, l1, *access, *mem_type, *seg_flags];
                data(&mut frames, &load_event(*lsm_idx, &event));

                // 2. Write the segment data (empty for RAM/stack segments).
                //    A_Memory_Write addresses 64 KiB; larger addresses would
                //    need A_MemoryExtended_Write.
                if !bytes.is_empty() && *address as usize + bytes.len() > 0x1_0000 {
                    bail!("step {}: segment at {address:#x} is above 64 KiB", i + 1);
                }
                for (n, chunk) in bytes.chunks(MEMORY_WRITE_CHUNK).enumerate() {
                    let offset = *address as usize + n * MEMORY_WRITE_CHUNK;
                    data(&mut frames, &apdu::memory_write(offset as u16, chunk));
                }
            }
            DownloadStep::TaskSegment { lsm_idx, address, pei_type, app_id } => {
                // AllocAbsTaskSeg: event 03h 02h, address, PEI type and the
                // application id (`03_05_02 §3.31.3.4`).
                let [a0, a1] = (*address as u16).to_be_bytes();
                let mut event = vec![0x03, 0x02, a0, a1, *pei_type];
                event.extend_from_slice(app_id);
                data(&mut frames, &load_event(*lsm_idx, &event));
            }
            DownloadStep::RelSegment { lsm_idx, size, mode, fill } => {
                // Data Relative Allocation: event 03h 0Bh, size (4), mode,
                // fill (`03_05_02 §3.31.3.4`).
                let mut event = vec![0x03, 0x0B];
                event.extend_from_slice(&size.to_be_bytes());
                event.extend_from_slice(&[*mode, *fill]);
                data(&mut frames, &load_event(*lsm_idx, &event));
            }
            DownloadStep::WriteRelMem { obj_idx, .. } => bail!(
                "step {}: LdCtrlWriteRelMem (object {obj_idx}) writes relative to the address \
                 the device reports in PID_TABLE_REFERENCE; it can only be encoded during a \
                 live download",
                i + 1
            ),
            DownloadStep::Delay { .. } | DownloadStep::MapError { .. } => {}
        }
    }

    Ok(frames)
}

/// One load-state-machine control write: `A_PropertyValue_Write` to
/// `PID_LOAD_STATE_CONTROL`, one element at index 1, the event padded to 10
/// octets.
fn load_event(lsm_idx: u8, event: &[u8]) -> Vec<u8> {
    let mut d = event.to_vec();
    d.resize(LOAD_EVENT_LEN, 0);
    apdu::property_value_write(lsm_idx, apci::PID_LOAD_STATE_CONTROL, 1, 1, &d)
}

/// `A_PropertyValue_Response` (`03_03_07 §3.4.3.2`).
const PROPERTY_VALUE_RESPONSE: u16 = 0x3D6;
/// `A_FilterTable_Read` / `_Response` (`03_03_07 §3.6.2`).
const FILTER_TABLE_READ: u16 = 0x3C1;
const FILTER_TABLE_RESPONSE: u16 = 0x3C2;
const PID_OBJECT_TYPE: u8 = 1;
pub(crate) const PID_TABLE_REFERENCE: u8 = 7;

/// Load states (`03_05_01 Table 92`).
const UNLOADED: u8 = 0;
pub(crate) const LOADED: u8 = 1;
const LOADING: u8 = 2;
const LOAD_COMPLETING: u8 = 5;
/// Longest a load state transition may take (`03_05_01 §4.23.2.1`).
const LOAD_STATE_TIMEOUT: Duration = Duration::from_secs(30);

/// Run a download plan over `t` to the device `dst` and check every answer:
/// the device descriptor must be `mask` before anything is loaded, property
/// comparisons must match, each load event must reach the state the load
/// procedure expects (`03_05_01 §4.23.2.4`), and every memory write is read
/// back, as ETS does for a device without `PID_DEVICE_CONTROL`. `log` gets a
/// line per step.
pub fn execute(t: &mut dyn Transport, plan: &[DownloadStep], dst: u16, mask: u16, log: &mut dyn FnMut(&str)) -> Result<()> {
    let Some((DownloadStep::Connect, rest)) = plan.split_first() else {
        bail!("the download plan does not start with Connect");
    };
    let mut c = Connection::open(t, dst).context("step 1: connect")?;
    let found = mgmt::device_descriptor_read(&mut c).context("step 1: device descriptor")?;
    if found != mask {
        bail!("the device has mask {found:04X}h, the application needs {mask:04X}h; nothing was loaded");
    }
    log(&format!("  1 connected, mask {found:04X}h"));
    let mut loaded = HashMap::new();
    for (i, step) in rest.iter().enumerate() {
        let n = i + 2;
        // The connection ends with Disconnect or Restart; only delays may
        // follow.
        let finished = || rest[i + 1..].iter().all(|s| matches!(s, DownloadStep::Disconnect | DownloadStep::Delay { .. }));
        match step {
            DownloadStep::Disconnect | DownloadStep::Restart if !finished() => {
                bail!("step {n}: steps after the end of the connection are not supported")
            }
            DownloadStep::Disconnect => {
                c.close()?;
                log(&format!("{n:3} disconnected"));
                return Ok(());
            }
            DownloadStep::Restart => {
                c.restart_and_abort().with_context(|| format!("step {n}: restart"))?;
                log(&format!("{n:3} restart"));
                return Ok(());
            }
            DownloadStep::Connect => bail!("step {n}: a second Connect is not supported"),
            _ => run_step(&mut c, n, step, &mut loaded, log).with_context(|| format!("step {n} ({})", step_name(step)))?,
        }
    }
    c.close()
}

/// One step; `loaded` holds the values of [`DownloadStep::ReadProp`] by
/// object index and PID.
fn run_step(
    c: &mut Connection,
    n: usize,
    step: &DownloadStep,
    loaded: &mut HashMap<(u8, u8), Vec<u8>>,
    log: &mut dyn FnMut(&str),
) -> Result<()> {
    match step {
        DownloadStep::CompareProp { obj_idx, prop_id, inline_data } => {
            let found = property_read(c, *obj_idx, *prop_id)?;
            // The inline data may be longer than the property (BE-02001.02:
            // 10 octets for the 6-octet hardware type).
            let len = found.len().min(inline_data.len());
            if found.is_empty() || found[..len] != inline_data[..len] {
                bail!(
                    "object {obj_idx} property {prop_id} is {}, the application expects {} (wrong device?)",
                    hex(&found),
                    hex(inline_data)
                );
            }
            log(&format!("{n:3} object {obj_idx} property {prop_id} = {} ok", hex(&found)));
        }
        DownloadStep::ReadProp { obj_idx, prop_id } => {
            let v = property_read(c, *obj_idx, *prop_id)?;
            log(&format!("{n:3} object {obj_idx} property {prop_id} = {}", hex(&v)));
            loaded.insert((*obj_idx, *prop_id), v);
        }
        DownloadStep::CompareLoadedProp { obj_idx, prop_id } => {
            let before = loaded.get(&(*obj_idx, *prop_id)).with_context(|| format!("object {obj_idx} property {prop_id} was not read before"))?;
            let now = property_read(c, *obj_idx, *prop_id)?;
            if now != *before {
                bail!("object {obj_idx} property {prop_id} is {}, was {} (the device moved the segment; a complete download is needed)", hex(&now), hex(before));
            }
            log(&format!("{n:3} object {obj_idx} property {prop_id} = {} unchanged", hex(&now)));
        }
        DownloadStep::WriteProp { object, prop_id, data, verify } => {
            let idx = object_index(c, object)?;
            let back = property_write(c, idx, *prop_id, data)?;
            if *verify && back != *data {
                bail!("object {idx} property {prop_id}: wrote {}, read {}", hex(data), hex(&back));
            }
            log(&format!("{n:3} object {idx} property {prop_id} := {}", hex(data)));
        }
        DownloadStep::Unload { object } => load(c, n, object, &[0x04], UNLOADED, log)?,
        DownloadStep::Load { object } => load(c, n, object, &[0x01], LOADING, log)?,
        DownloadStep::LoadCompleted { object } => load(c, n, object, &[0x02], LOADED, log)?,
        DownloadStep::AbsSegment { lsm_idx, seg_type, address, size, access, mem_type, seg_flags, data } => {
            let [a0, a1] = (*address as u16).to_be_bytes();
            let [l0, l1] = (*size as u16).to_be_bytes();
            let event = [0x03, *seg_type, a0, a1, l0, l1, *access, *mem_type, *seg_flags];
            load(c, n, &Object::Index(*lsm_idx), &event, LOADING, log)?;
            write_memory(c, *address as u16, data)?;
            if !data.is_empty() {
                log(&format!("{n:3} memory {address:04X}h: {} octet(s) written and read back", data.len()));
            }
        }
        DownloadStep::TaskSegment { lsm_idx, address, pei_type, app_id } => {
            let [a0, a1] = (*address as u16).to_be_bytes();
            let mut event = vec![0x03, 0x02, a0, a1, *pei_type];
            event.extend_from_slice(app_id);
            load(c, n, &Object::Index(*lsm_idx), &event, LOADING, log)?;
        }
        DownloadStep::RelSegment { lsm_idx, size, mode, fill } => {
            let mut event = vec![0x03, 0x0B];
            event.extend_from_slice(&size.to_be_bytes());
            event.extend_from_slice(&[*mode, *fill]);
            load(c, n, &Object::Index(*lsm_idx), &event, LOADING, log)?;
        }
        DownloadStep::WriteMem { address, data, .. } => {
            write_memory(c, *address, data)?;
            log(&format!("{n:3} memory {address:04X}h: {} octet(s) written and read back", data.len()));
        }
        DownloadStep::WriteRelMem { obj_idx, offset, data, .. } => {
            let reference = property_read(c, *obj_idx, PID_TABLE_REFERENCE)?;
            let base = reference.iter().fold(0u64, |a, b| a << 8 | *b as u64);
            let address = base + *offset as u64;
            if reference.is_empty() || address + data.len() as u64 > 0x1_0000 {
                bail!("object {obj_idx}: table reference {} + {offset} is not a 16-bit address", hex(&reference));
            }
            write_memory(c, address as u16, data)?;
            log(&format!("{n:3} memory {address:04X}h (object {obj_idx} + {offset}): {} octet(s)", data.len()));
        }
        DownloadStep::ClearFilterTable { object } => {
            // SRVID_CLEAR_FILTERTABLE (`03_05_01 §4.5.7.2`): the answer is
            // return code 00h and the service id.
            let idx = object_index(c, object)?;
            let cmd = apdu::function_property_command(idx, apci::PID_ROUTETABLE_CONTROL, &[0x00, apci::SRVID_CLEAR_FILTERTABLE]);
            let resp = c.request(&cmd, apci::FUNCTION_PROPERTY_STATE_RESPONSE)?;
            if apdu::apci_of(&resp) != Some(apci::FUNCTION_PROPERTY_STATE_RESPONSE)
                || resp.get(2..6) != Some(&[idx, apci::PID_ROUTETABLE_CONTROL, 0x00, apci::SRVID_CLEAR_FILTERTABLE][..])
            {
                bail!("clearing the filter table failed: {}", hex(&resp));
            }
            log(&format!("{n:3} object {idx}: filter table cleared"));
        }
        DownloadStep::WriteFilterTable { address, data, cleared, .. } => {
            // DMP_LCExtMemWrite_RCo (`03_05_02 §3.41.2`): open, then writes of
            // up to 11 octets, each read back. After a clear only the non-zero
            // octets are written.
            c.send(&apdu::filter_table_open())?;
            let writes = filter_table_writes(data, *cleared, FILTER_TABLE_WRITE_CHUNK);
            let written: usize = writes.iter().map(|(_, w)| w.len()).sum();
            for (offset, chunk) in writes {
                let at = *address as usize + offset;
                if at + chunk.len() > 0x1_0000 {
                    bail!("filter table write at {at:04X}h goes above 64 KiB");
                }
                let at = at as u16;
                c.send(&apdu::filter_table_write(at, chunk))?;
                let read = [0x03, (FILTER_TABLE_READ & 0xFF) as u8, chunk.len() as u8, (at >> 8) as u8, at as u8];
                let resp = c.request(&read, FILTER_TABLE_RESPONSE)?;
                if apdu::apci_of(&resp) != Some(FILTER_TABLE_RESPONSE) || resp.get(2..5) != Some(&read[2..5]) || resp[5..] != *chunk {
                    bail!("filter table {at:04X}h: wrote {}, read back {}", hex(chunk), hex(&resp));
                }
            }
            log(&format!("{n:3} filter table {address:04X}h: {written} octet(s) written and read back"));
        }
        DownloadStep::Delay { ms } => std::thread::sleep(Duration::from_millis(*ms as u64)),
        DownloadStep::MapError { .. } => {}
        DownloadStep::Connect | DownloadStep::Disconnect | DownloadStep::Restart => unreachable!("handled by execute"),
    }
    Ok(())
}

fn step_name(step: &DownloadStep) -> String {
    let s = format!("{step:?}");
    s.split([' ', '{', '(']).next().unwrap_or_default().to_string()
}

/// `A_PropertyValue_Read` of one element at index 1; the value, empty for
/// an error answer.
pub(crate) fn property_read(c: &mut Connection, obj: u8, pid: u8) -> Result<Vec<u8>> {
    let resp = c.request(&apdu::property_value_read(obj, pid, 1, 1), PROPERTY_VALUE_RESPONSE)?;
    property_value(&resp, obj, pid)
}

/// `A_PropertyValue_Write` of one element at index 1; the value the device
/// answers with. An error answer is an error.
fn property_write(c: &mut Connection, obj: u8, pid: u8, data: &[u8]) -> Result<Vec<u8>> {
    let resp = c.request(&apdu::property_value_write(obj, pid, 1, 1, data), PROPERTY_VALUE_RESPONSE)?;
    let v = property_value(&resp, obj, pid)?;
    if resp[4] >> 4 == 0 {
        bail!("object {obj} property {pid}: the device refused the write");
    }
    Ok(v)
}

fn property_value(resp: &[u8], obj: u8, pid: u8) -> Result<Vec<u8>> {
    if apdu::apci_of(resp) != Some(PROPERTY_VALUE_RESPONSE) || resp.len() < 6 || resp[2] != obj || resp[3] != pid {
        bail!("unexpected answer to a property access of object {obj} property {pid}: {}", hex(resp));
    }
    Ok(resp[6..].to_vec())
}

/// The index of an interface object named by type: the first object whose
/// `PID_OBJECT_TYPE` matches.
pub(crate) fn object_index(c: &mut Connection, object: &Object) -> Result<u8> {
    let kind = match object {
        Object::Index(i) => return Ok(*i),
        Object::Type(t) => *t,
    };
    for idx in 0..=u8::MAX {
        let v = property_read(c, idx, PID_OBJECT_TYPE)?;
        if v.is_empty() {
            break;
        }
        if v.iter().fold(0u32, |a, b| a << 8 | *b as u32) == kind as u32 {
            return Ok(idx);
        }
    }
    bail!("the device has no interface object of type {kind}")
}

/// A load event and the state it must lead to; `LoadCompleting` is waited
/// out by reading the state (`03_05_01 §4.23.2.4`).
fn load(c: &mut Connection, n: usize, object: &Object, event: &[u8], want: u8, log: &mut dyn FnMut(&str)) -> Result<()> {
    let idx = object_index(c, object)?;
    let mut d = event.to_vec();
    d.resize(LOAD_EVENT_LEN, 0);
    let mut state = *property_write(c, idx, apci::PID_LOAD_STATE_CONTROL, &d)?.first().unwrap_or(&0xFF);
    let deadline = Instant::now() + LOAD_STATE_TIMEOUT;
    while state == LOAD_COMPLETING && want == LOADED && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        state = *property_read(c, idx, apci::PID_LOAD_STATE_CONTROL)?.first().unwrap_or(&0xFF);
    }
    if state != want {
        bail!("object {idx}: load event {} gave load state {state}, expected {want}", hex(event));
    }
    log(&format!("{n:3} object {idx} load event {} -> state {state}", hex(event)));
    Ok(())
}

/// `A_Memory_Write` in standard-frame chunks, each read back and compared.
fn write_memory(c: &mut Connection, address: u16, data: &[u8]) -> Result<()> {
    if address as usize + data.len() > 0x1_0000 {
        bail!("write at {address:04X}h+{} goes above 64 KiB", data.len());
    }
    for (k, chunk) in data.chunks(MEMORY_WRITE_CHUNK).enumerate() {
        let at = address + (k * MEMORY_WRITE_CHUNK) as u16;
        c.send(&apdu::memory_write(at, chunk))?;
        let back = mgmt::memory_read(c, at, chunk.len())?;
        if back != chunk {
            bail!("memory {at:04X}h: wrote {}, read back {}", hex(chunk), hex(&back));
        }
    }
    Ok(())
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The TPDU of a frame: TPCI/APCI octet and the rest.
    fn tpdu(f: &CemiFrame) -> &[u8] {
        // Ctrl1 Ctrl2 Src(2) Dst(2) L, then the TPDU.
        &f.body[7..]
    }

    #[test]
    fn connect_disconnect_restart_sequence() {
        let plan = [DownloadStep::Connect, DownloadStep::Restart, DownloadStep::Disconnect];
        let frames = plan_to_frames(&plan, 0x1001).unwrap();
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].message_code, knx_transport::message_code::L_DATA_REQ);
        assert_eq!(tpdu(&frames[0]), &[0x80]);
        // A_Restart on the first connected sequence number.
        assert_eq!(tpdu(&frames[1]), &[0x43, 0x80]);
        assert_eq!(tpdu(&frames[2]), &[0x81]);
    }

    #[test]
    fn abs_segment_emits_descriptor_and_chunked_memory_writes() {
        let plan = [DownloadStep::AbsSegment {
            lsm_idx: 1,
            seg_type: 0,
            address: 0x4000,
            size: 40,
            access: 0xFF,
            mem_type: 3,
            seg_flags: 0x80,
            data: vec![0xAA; 40],
        }];
        let frames = plan_to_frames(&plan, 0x1001).unwrap();
        // 1 load event + ceil(40/12) = 4 memory writes.
        assert_eq!(frames.len(), 5);
        // A_PropertyValue_Write(obj 1, PID 5, 1 elem @1, 03 00 4000 0028 FF 03 80 00).
        assert_eq!(
            tpdu(&frames[0]),
            &[0x43, 0xD7, 0x01, 0x05, 0x10, 0x01, 0x03, 0x00, 0x40, 0x00, 0x00, 0x28, 0xFF, 0x03, 0x80, 0x00]
        );
        // L field: 15 octets after the TPCI octet (standard frame maximum).
        assert_eq!(frames[0].body[6], 15);
        // First memory write: 12 octets at 0x4000, sequence 1.
        assert_eq!(&tpdu(&frames[1])[..4], &[0x46, 0x8C, 0x40, 0x00]);
    }

    #[test]
    fn ram_segments_are_only_allocated() {
        let plan = [DownloadStep::AbsSegment {
            lsm_idx: 3,
            seg_type: 0,
            address: 0x0700,
            size: 82,
            access: 0,
            mem_type: 2,
            seg_flags: 0,
            data: vec![],
        }];
        assert_eq!(plan_to_frames(&plan, 0x1001).unwrap().len(), 1);
    }

    #[test]
    fn task_segment_carries_application_id() {
        let plan = [DownloadStep::TaskSegment {
            lsm_idx: 3,
            address: 0x4400,
            pei_type: 1,
            app_id: [0x00, 0x83, 0x00, 0xB6, 0x32],
        }];
        let f = plan_to_frames(&plan, 0x1001).unwrap();
        assert_eq!(
            &tpdu(&f[0])[6..],
            &[0x03, 0x02, 0x44, 0x00, 0x01, 0x00, 0x83, 0x00, 0xB6, 0x32]
        );
    }

    #[test]
    fn filter_table_is_opened_and_written_in_11_octet_chunks() {
        let plan = [DownloadStep::WriteFilterTable { address: 0x0200, data: vec![0xAA; 23], verify: true, cleared: false }];
        let frames = plan_to_frames(&plan, 0x1000).unwrap();
        assert_eq!(frames.len(), 4);
        assert_eq!(tpdu(&frames[0])[1], 0xC0); // A_FilterTable_Open
        let first = tpdu(&frames[1]);
        assert_eq!(first[0] & 0x03, 0x03);
        assert_eq!(&first[1..5], &[0xC3, 11, 0x02, 0x00]);
        assert_eq!(&tpdu(&frames[2])[1..5], &[0xC3, 11, 0x02, 0x0B]);
        assert_eq!(&tpdu(&frames[3])[1..5], &[0xC3, 1, 0x02, 0x16]);

        // After a clear only the non-zero octets, as ETS 5.7 writes them.
        let mut data = vec![0u8; 8192];
        data[1] = 0x0C;
        let plan = [DownloadStep::WriteFilterTable { address: 0x0200, data, verify: true, cleared: true }];
        let frames = plan_to_frames(&plan, 0x1000).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(&tpdu(&frames[1])[1..6], &[0xC3, 1, 0x02, 0x01, 0x0C]);
    }

    #[test]
    fn objects_by_type_need_a_live_download() {
        let plan = [DownloadStep::Load { object: Object::Type(6) }];
        let err = plan_to_frames(&plan, 0x1000).unwrap_err().to_string();
        assert!(err.contains("by type (6)"), "{err}");
        let plan = [DownloadStep::ClearFilterTable { object: Object::Index(1) }];
        let frames = plan_to_frames(&plan, 0x1000).unwrap();
        let t = tpdu(&frames[0]);
        assert_eq!(t[0] & 0x03, 0x02); // APCI bits 9..8, behind the TPCI
        assert_eq!(&t[1..], &[0xC7, 1, 56, 0x00, 0x01]);
    }
}

/// [`execute`] against knx-sim's simulated BE-02001.02 over KNXnet/IP on
/// localhost.
#[cfg(test)]
mod live_tests {
    use super::*;
    use knx_sim::bus::{Bus, Log};
    use knx_sim::device::{load_state, Device};
    use knx_sim::knxip::Description;
    use knx_sim::server::{Input, Server};
    use knx_transport::ip::IpTransport;
    use std::collections::HashMap;
    use std::path::PathBuf;

    const H2P: &str = "M-0083_H-182-0_HP-00B6-32-9642";

    fn knxprod() -> Option<PathBuf> {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../refs/knxprod/MDT_KP_BE_02_Universal_Interface_V32a.knxprod");
        p.is_file().then_some(p)
    }

    /// The default download of the BE-02001.02 with one link.
    fn plan(path: &PathBuf) -> Vec<DownloadStep> {
        let kp = knx_prod::KnxProd::open(path).unwrap();
        let ap = kp.application_program(H2P).unwrap();
        let values = HashMap::new();
        let ev = knx_prod::Evaluation::new(&ap, &values);
        let links = [knx_prod::tables::Link::new(0, 0x000A)];
        let image = knx_prod::device_image::build_device_image_eval(&ev, kp.table_format(&ap).unwrap(), &links).unwrap();
        knx_prod::download_plan(&ev, &image, None, knx_prod::DownloadKind::Full).unwrap()
    }

    /// Run `plan` against `dev`; the result and the device afterwards.
    fn run(dev: Device, plan: &[DownloadStep]) -> (Result<()>, Device) {
        let desc = Description { individual_address: 0x11FA, serial: [0; 6], mac: [0; 6], name: "t".into() };
        let mut server = Server::new("127.0.0.1:0".parse().unwrap(), desc, 0x11FB, Bus::new(vec![dev], Log::to_file(None))).unwrap();
        let addr = server.local_addr();
        let mask = server.bus.devices[0].mask;
        let dst = server.bus.devices[0].address;
        let (tx, rx) = std::sync::mpsc::channel();
        let t = std::thread::spawn(move || {
            server.run(rx).unwrap();
            server
        });
        let result = {
            let mut ip = IpTransport::connect(addr).unwrap();
            execute(&mut ip, plan, dst, mask, &mut |_| {})
        };
        tx.send(Input::Command("quit".into())).unwrap();
        let mut server = t.join().unwrap();
        (result, server.bus.devices.remove(0))
    }

    #[test]
    fn download_into_simulated_device() {
        let Some(path) = knxprod() else { return };
        let dev = knx_sim::profile::device(&path, H2P, 0x1101).unwrap();
        let (result, dev) = run(dev, &plan(&path));
        result.unwrap();
        let dump = dev.dump();
        for lsm in 1..=3 {
            assert_eq!(dev.objects[lsm].lsm.as_ref().unwrap().state, load_state::LOADED, "{dump}");
        }
        assert!(dump.contains("  run state running\n"), "{dump}");
        assert!(dump.contains("  pid 13: 00 83 00 B6 32\n"), "{dump}");
        assert!(dump.contains("  4000: 02 -- -- 00 0A --"), "{dump}");
        assert!(dump.contains("  4200: -- 01 01 00 --"), "{dump}");
    }

    /// The AKU-02UP.03 (System B): complete, then partial download.
    #[test]
    fn system_b_complete_and_partial() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../refs/knxprod/MDT_KP_AKU_03_Universal_Actuator_V41a.knxprod");
        if !path.is_file() {
            return;
        }
        const AKU: &str = "M-0083_H-287-1_HP-00C5-41-204E";
        let kp = knx_prod::KnxProd::open(&path).unwrap();
        let ap = kp.application_program(AKU).unwrap();
        let master = kp.master_data().unwrap();
        let values = HashMap::new();
        let ev = knx_prod::Evaluation::new(&ap, &values);
        let image = knx_prod::device_image::build_device_image_eval(&ev, kp.table_format(&ap).unwrap(), &[]).unwrap();
        let mut plan = knx_prod::download_plan(&ev, &image, Some(&master), knx_prod::DownloadKind::Full).unwrap();
        // Both downloads over one simulated connection: the partial one
        // follows the complete one (its Restart ends the first part).
        let partial = knx_prod::download_plan(&ev, &image, Some(&master), knx_prod::DownloadKind::Partial).unwrap();
        let dev = knx_sim::profile::device(&path, AKU, 0x1101).unwrap();
        let (result, dev) = run(dev, &plan);
        result.unwrap();
        let reference = |d: &Device| d.objects[4].properties.iter().find(|p| p.id == 7).unwrap().elements[0].clone();
        let before = reference(&dev);
        assert_ne!(before, [0, 0, 0, 0]);
        plan = partial;
        let (result, dev) = run(dev, &plan);
        result.unwrap();
        assert_eq!(reference(&dev), before);
        for lsm in 1..=4 {
            assert_eq!(dev.objects[lsm].lsm.as_ref().unwrap().state, load_state::LOADED);
        }
        assert_eq!(dev.objects[5].lsm.as_ref().unwrap().state, load_state::UNLOADED);
    }

    /// A device of a project and how ETS 5.7 left it in knx-sim.
    struct EtsCase<'a> {
        knxprod: &'a str,
        h2p: &'a str,
        address: u16,
        values: &'a [(&'a str, &'a str)],
        /// `(object, group address, sending)`
        links: &'a [(u16, u16, bool)],
        overrides: &'a [knx_prod::tables::ObjectOverride],
        /// Dump of all devices after ETS's download (`refs/ets5`).
        reference: &'a str,
    }

    /// knxcfg's download of the same configuration leaves the device as
    /// ETS's did.
    fn same_state_as_ets(case: &EtsCase) {
        let refs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../refs");
        let (path, reference) = (refs.join("knxprod").join(case.knxprod), refs.join("ets5").join(case.reference));
        if !path.is_file() || !reference.is_file() {
            eprintln!("skipping: {} or {} not found", path.display(), reference.display());
            return;
        }
        let kp = knx_prod::KnxProd::open(&path).unwrap();
        let ap = kp.application_program(case.h2p).unwrap();
        let values: HashMap<String, String> =
            case.values.iter().map(|(k, v)| (knx_prod::eval::full_key(&ap, k), v.to_string())).collect();
        let ev = knx_prod::Evaluation::new(&ap, &values);
        let links: Vec<_> = case.links.iter().map(|&(asap, ga, send)| knx_prod::tables::Link { asap, ga, send }).collect();
        let image = knx_prod::device_image::build_device_image_with(&ev, kp.table_format(&ap).unwrap(), &links, case.overrides).unwrap();
        let master = kp.master_data().unwrap();
        let merged = ap.load_procedure_style == "MergedProcedure";
        let plan = knx_prod::download_plan(&ev, &image, merged.then_some(&master), knx_prod::DownloadKind::Full).unwrap();
        let dev = knx_sim::profile::device(&path, case.h2p, case.address).unwrap();
        let (result, dev) = run(dev, &plan);
        result.unwrap();

        let text = std::fs::read_to_string(reference).unwrap();
        let header = format!("device {}.{}.{}", case.address >> 12, (case.address >> 8) & 0xF, case.address & 0xFF);
        let start = text.find(&format!("{header}\n")).unwrap_or_else(|| panic!("{header} not in {}", case.reference));
        let end = text[start + 1..].find("\ndevice ").map_or(text.len(), |e| start + 2 + e);
        assert_eq!(dev.dump(), text[start..end], "{header}");
    }

    const BE: &str = "MDT_KP_BE_02_Universal_Interface_V32a.knxprod";
    const AKU_KNXPROD: &str = "MDT_KP_AKU_03_Universal_Actuator_V41a.knxprod";
    const AKU_H2P: &str = "M-0083_H-287-1_HP-00C5-41-204E";

    /// Flags and priorities changed per object in the knx-sim project
    /// (docs/ets5-findings.md).
    #[test]
    fn object_settings_like_ets() {
        use knx_prod::tables::ObjectOverride;
        same_state_as_ets(&EtsCase {
            knxprod: BE,
            h2p: H2P,
            address: 0x1101,
            values: &[],
            links: &[(0, 0x000A, false), (0, 0x000B, true)],
            overrides: &[
                ObjectOverride { asap: 0, read: Some(false), write: Some(true), update: Some(true), priority: Some(0b01), ..Default::default() },
                ObjectOverride { asap: 6, transmit: Some(false), priority: Some(0b10), ..Default::default() },
            ],
            reference: "knx-sim-flags-ets-dump.txt",
        });
        same_state_as_ets(&EtsCase {
            knxprod: AKU_KNXPROD,
            h2p: AKU_H2P,
            address: 0x1102,
            values: &[("P-12", "5")],
            links: &[],
            overrides: &[
                ObjectOverride { asap: 1, read: Some(true), transmit: Some(true), read_on_init: Some(true), priority: Some(0b10), ..Default::default() },
                ObjectOverride { asap: 3, write: Some(false), update: Some(true), ..Default::default() },
            ],
            reference: "knx-sim-flags-ets-dump.txt",
        });
    }

    /// A project made with knxcfg, exported and downloaded by ETS
    /// (docs/knxproj-format.md, "Export"): module parameters, a text
    /// parameter, links and object settings.
    #[test]
    fn exported_project_like_ets() {
        use knx_prod::tables::ObjectOverride;
        same_state_as_ets(&EtsCase {
            knxprod: BE,
            h2p: H2P,
            address: 0x1101,
            values: &[("P-7", "50"), ("MD-2_M-2/MD-2_P-23", "Kueche"), ("MD-2_M-3/MD-2_P-24", "1")],
            links: &[(0, 0x0A03, false), (0, 0x1900, false)],
            overrides: &[ObjectOverride { asap: 0, read: Some(false), ..Default::default() }],
            reference: "knxcfg-project-ets-dump.txt",
        });
        same_state_as_ets(&EtsCase {
            knxprod: AKU_KNXPROD,
            h2p: AKU_H2P,
            address: 0x1102,
            values: &[],
            links: &[(8, 0x0A03, false)],
            overrides: &[ObjectOverride { asap: 8, priority: Some(0b01), ..Default::default() }],
            reference: "knxcfg-project-ets-dump.txt",
        });
    }

    #[test]
    fn wrong_hardware_type_loads_nothing() {
        let Some(path) = knxprod() else { return };
        let mut dev = knx_sim::profile::device(&path, H2P, 0x1101).unwrap();
        dev.set_property(0, 78, &[0, 0, 0, 0, 0x02, 0xAD]);
        let (result, dev) = run(dev, &plan(&path));
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("wrong device"), "{err}");
        assert!(!dev.dump().contains("memory written\n  "), "{}", dev.dump());
        assert_eq!(dev.objects[3].lsm.as_ref().unwrap().state, load_state::UNLOADED);
    }
}
