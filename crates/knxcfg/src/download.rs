//! Convert a knxprod download plan into cEMI management frames.
//!
//! This is the glue between `knx-prod`'s `download_plan` (the abstract
//! `<LoadProcedure>` sequence with assembled segment bytes) and
//! `knx-transport`'s wire encoding (cEMI `L_Data` frames carrying the `A_*`
//! management APDUs). It lives in the CLI crate because the layers are strict:
//! `knx-prod` (commissioning) and `knx-transport` (protocol) both stay below
//! the orchestrator — see `docs/plan.md` §4.
//!
//! The result is the request side of the conversation only. Steps whose
//! frames depend on a device response (`LdCtrlWriteRelMem` needs the
//! object's `PID_TABLE_REFERENCE` first) cannot be pre-encoded and are
//! reported as errors.

use anyhow::{bail, Result};
use knx_prod::DownloadStep;
use knx_transport::apdu::{self, apci};
use knx_transport::cemi::{self, tpci};
use knx_transport::CemiFrame;

/// Data octets per `A_Memory_Write` in a standard frame: 15 TPDU octets
/// after the TPCI octet, minus APCI and address (`03_03_07 §3.5.4`).
const MEMORY_WRITE_CHUNK: usize = 12;

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
            DownloadStep::ReadProp { obj_idx, prop_id } => {
                data(&mut frames, &apdu::property_value_read(*obj_idx, *prop_id, 1, 1));
            }
            DownloadStep::WriteProp { obj_idx, prop_id, data: d, .. } => {
                data(&mut frames, &apdu::property_value_write(*obj_idx, *prop_id, 1, 1, d));
            }
            DownloadStep::Unload { lsm_idx } => data(&mut frames, &load_event(*lsm_idx, &[0x04])),
            DownloadStep::Load { lsm_idx } => data(&mut frames, &load_event(*lsm_idx, &[0x01])),
            DownloadStep::LoadCompleted { lsm_idx } => {
                data(&mut frames, &load_event(*lsm_idx, &[0x02]))
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
}
