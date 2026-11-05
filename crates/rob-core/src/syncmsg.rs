//! ROB-Sync messages (spec, "Clock sync").
//!
//! A follower sends `SYNC_REQ` stamped with its local time `t1`; the leader
//! answers with `SYNC_RESP` carrying `t1` back plus its own receive time `t2` and
//! send time `t3`. The follower records `t4` on arrival. All times are
//! nanoseconds on the sender's own clock, big-endian on the wire.
//!
//! ```text
//! SYNC_REQ    (28 bytes): "AMTS" ver kind=1 rsvd[2] node_id[8] seq[4] t1[8]
//! SYNC_RESP   (48 bytes): "AMTS" ver kind=2 rsvd[2] leader_id[8] seq[4]
//!                         t1[8] t2[8] t3[8] class[1] priority[1] rsvd[2]
//! SYNC_REPORT (36 bytes): "AMTS" ver kind=3 rsvd[2] node_id[8] seq[4]
//!                         sender_ns[8] engine_ns[8]
//! ```
//!
//! `class` says how good the leader's own time is, and `priority` is an operator
//! tie-breaker; followers prefer the lowest `(class, priority, leader_id)`.
//!
//! A node sends `SYNC_REPORT` (fire-and-forget, about once a second) so the
//! engine can learn each input's path floor and line inputs up (design 26, S4):
//! it is one calibration sample — at the node's own clock reading `sender_ns`
//! (the clock its audio is stamped in), the engine's time was about `engine_ns`,
//! from the node's two-way estimate. Old builds ignore an unknown kind, so the
//! message is additive and does not bump [`VERSION`].

use crate::Error;

pub const MAGIC: [u8; 4] = *b"AMTS";
pub const VERSION: u8 = 1;
pub const KIND_REQ: u8 = 1;
pub const KIND_RESP: u8 = 2;
pub const KIND_REPORT: u8 = 3;
pub const REQ_LEN: usize = 28;
pub const RESP_LEN: usize = 48;
pub const REPORT_LEN: usize = 36;

/// Clock classes (lower is better). Values leave room between them for finer
/// classes later; compare them numerically.
pub mod class {
    /// Locked to an external time or frequency reference: a PTP grandmaster,
    /// GPS, or an audio interface slaved to word clock.
    pub const EXTERNAL_LOCKED: u8 = 0;
    /// Was locked to an external reference that has been lost; free-running on
    /// the last good frequency.
    pub const EXTERNAL_HOLDOVER: u8 = 64;
    /// Free-running on its own oscillator: no external reference.
    pub const INTERNAL: u8 = 128;
    /// Following another leader and ready to take over if it fails.
    pub const BACKUP: u8 = 192;
    /// Must not be followed.
    pub const UNUSABLE: u8 = 255;
}

/// Default operator priority (lower wins ties within a class).
pub const DEFAULT_PRIORITY: u8 = 128;

/// `SYNC_REQ`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncReq {
    pub node_id: u64,
    pub seq: u32,
    /// Follower local time at send.
    pub t1: u64,
}

/// `SYNC_RESP`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncResp {
    pub leader_id: u64,
    pub seq: u32,
    /// Follower's `t1`, echoed.
    pub t1: u64,
    /// Leader time at receive.
    pub t2: u64,
    /// Leader time at send.
    pub t3: u64,
    /// A [`class`] value: how good the leader's own time is.
    pub class: u8,
    /// Operator tie-breaker within a class (lower wins).
    pub priority: u8,
}

/// `SYNC_REPORT`: a node's two-way estimate, as one calibration sample
/// (design 26, S4). Fire-and-forget, about once a second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncReport {
    pub node_id: u64,
    pub seq: u32,
    /// The node's own clock reading at report time (the clock its audio is
    /// stamped in).
    pub sender_ns: u64,
    /// The node's estimate of engine time at that reading, from its two-way
    /// sync. Noisy; averaged by the receiver over minutes, never trusted singly.
    pub engine_ns: u64,
}

/// Either message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Msg {
    Req(SyncReq),
    Resp(SyncResp),
    Report(SyncReport),
}

fn header(out: &mut [u8], kind: u8) {
    out[0..4].copy_from_slice(&MAGIC);
    out[4] = VERSION;
    out[5] = kind;
    out[6..8].fill(0);
}

/// Writes a request. Returns bytes written ([`REQ_LEN`]).
pub fn write_req(m: &SyncReq, out: &mut [u8]) -> Result<usize, Error> {
    if out.len() < REQ_LEN {
        return Err(Error::OutputTooSmall);
    }
    header(out, KIND_REQ);
    out[8..16].copy_from_slice(&m.node_id.to_be_bytes());
    out[16..20].copy_from_slice(&m.seq.to_be_bytes());
    out[20..28].copy_from_slice(&m.t1.to_be_bytes());
    Ok(REQ_LEN)
}

/// Writes a response. Returns bytes written ([`RESP_LEN`]).
pub fn write_resp(m: &SyncResp, out: &mut [u8]) -> Result<usize, Error> {
    if out.len() < RESP_LEN {
        return Err(Error::OutputTooSmall);
    }
    header(out, KIND_RESP);
    out[8..16].copy_from_slice(&m.leader_id.to_be_bytes());
    out[16..20].copy_from_slice(&m.seq.to_be_bytes());
    out[20..28].copy_from_slice(&m.t1.to_be_bytes());
    out[28..36].copy_from_slice(&m.t2.to_be_bytes());
    out[36..44].copy_from_slice(&m.t3.to_be_bytes());
    out[44] = m.class;
    out[45] = m.priority;
    out[46..48].fill(0);
    Ok(RESP_LEN)
}

/// Writes a report. Returns bytes written ([`REPORT_LEN`]).
pub fn write_report(m: &SyncReport, out: &mut [u8]) -> Result<usize, Error> {
    if out.len() < REPORT_LEN {
        return Err(Error::OutputTooSmall);
    }
    header(out, KIND_REPORT);
    out[8..16].copy_from_slice(&m.node_id.to_be_bytes());
    out[16..20].copy_from_slice(&m.seq.to_be_bytes());
    out[20..28].copy_from_slice(&m.sender_ns.to_be_bytes());
    out[28..36].copy_from_slice(&m.engine_ns.to_be_bytes());
    Ok(REPORT_LEN)
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[i..i + 8]);
    u64::from_be_bytes(a)
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// Parses either message, or `None` if the datagram is not an ROB-Sync message
/// of a version we understand.
pub fn parse(buf: &[u8]) -> Option<Msg> {
    if buf.len() < 8 || buf[0..4] != MAGIC || buf[4] != VERSION {
        return None;
    }
    match buf[5] {
        KIND_REQ if buf.len() >= REQ_LEN => Some(Msg::Req(SyncReq {
            node_id: u64_at(buf, 8),
            seq: u32_at(buf, 16),
            t1: u64_at(buf, 20),
        })),
        KIND_RESP if buf.len() >= RESP_LEN => Some(Msg::Resp(SyncResp {
            leader_id: u64_at(buf, 8),
            seq: u32_at(buf, 16),
            t1: u64_at(buf, 20),
            t2: u64_at(buf, 28),
            t3: u64_at(buf, 36),
            class: buf[44],
            priority: buf[45],
        })),
        KIND_REPORT if buf.len() >= REPORT_LEN => Some(Msg::Report(SyncReport {
            node_id: u64_at(buf, 8),
            seq: u32_at(buf, 16),
            sender_ns: u64_at(buf, 20),
            engine_ns: u64_at(buf, 28),
        })),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips() {
        let m = SyncReq {
            node_id: 0xDEAD_BEEF_0102_0304,
            seq: 7,
            t1: u64::MAX - 5,
        };
        let mut b = [0u8; 64];
        assert_eq!(write_req(&m, &mut b).unwrap(), REQ_LEN);
        assert_eq!(parse(&b[..REQ_LEN]), Some(Msg::Req(m)));
    }

    #[test]
    fn response_round_trips() {
        let m = SyncResp {
            leader_id: 9,
            seq: 0xFFFF_FFFF,
            t1: 1,
            t2: 2,
            t3: 3,
            class: class::BACKUP,
            priority: 7,
        };
        let mut b = [0u8; 64];
        assert_eq!(write_resp(&m, &mut b).unwrap(), RESP_LEN);
        assert_eq!(parse(&b[..RESP_LEN]), Some(Msg::Resp(m)));
    }

    #[test]
    fn report_round_trips() {
        let m = SyncReport {
            node_id: 0x0102_0304_0506_0708,
            seq: 12,
            sender_ns: u64::MAX - 7,
            engine_ns: 0x0A0B_0C0D_0E0F_1011,
        };
        let mut b = [0u8; 64];
        assert_eq!(write_report(&m, &mut b).unwrap(), REPORT_LEN);
        assert_eq!(parse(&b[..REPORT_LEN]), Some(Msg::Report(m)));
        // Truncated reports are refused, like the other kinds.
        assert_eq!(parse(&b[..REPORT_LEN - 1]), None);
    }

    #[test]
    fn rejects_foreign_short_and_future_versions() {
        assert_eq!(parse(&[0u8; 64]), None);
        assert_eq!(parse(b"AMTS"), None);
        let mut b = [0u8; 64];
        let m = SyncReq {
            node_id: 1,
            seq: 1,
            t1: 1,
        };
        write_req(&m, &mut b).unwrap();
        assert_eq!(parse(&b[..REQ_LEN - 1]), None, "truncated");
        b[4] = VERSION + 1;
        assert_eq!(parse(&b[..REQ_LEN]), None, "future version");
    }

    #[test]
    fn small_buffers_are_refused() {
        let m = SyncReq {
            node_id: 1,
            seq: 1,
            t1: 1,
        };
        assert_eq!(write_req(&m, &mut [0u8; 10]), Err(Error::OutputTooSmall));
    }

    /// ROB rename (2026-10-14): names changed, the wire did not. Older builds on
    /// the field rig must still understand these bytes.
    #[test]
    fn the_wire_magic_is_unchanged_by_the_rob_rename() {
        assert_eq!(MAGIC, *b"AMTS");
    }
}
