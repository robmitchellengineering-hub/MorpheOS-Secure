//! PTPv2 (IEEE 1588-2008) messages over UDP, as AES67, ST 2110 and Dante in
//! AES67 mode use them: Sync, Follow_Up, Delay_Req, Delay_Resp, Announce.
//! Parsing and writing only; the protocol lives in `rob-sync::ptp`.
//!
//! Event messages (Sync, Delay_Req) go to UDP port 319, general ones to 320,
//! multicast 224.0.1.129 by default. Times are PTP timestamps (seconds and
//! nanoseconds since the PTP epoch, TAI); the correction field is in
//! nanoseconds times 2^16.

use core::fmt;

pub const EVENT_PORT: u16 = 319;
pub const GENERAL_PORT: u16 = 320;
pub const MULTICAST_V4: [u8; 4] = [224, 0, 1, 129];
pub const HEADER_LEN: usize = 34;
/// flagField: two-step (a Follow_Up carries the precise Sync time).
pub const FLAG_TWO_STEP: u16 = 0x0200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Short,
    Version,
    Unknown(u8),
    Space,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Short => write!(f, "PTP message too short"),
            Error::Version => write!(f, "not PTP version 2"),
            Error::Unknown(t) => write!(f, "PTP message type {t} not handled"),
            Error::Space => write!(f, "buffer too small"),
        }
    }
}

impl core::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PortIdentity {
    pub clock: [u8; 8],
    pub port: u16,
}

/// A PTP timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, PartialOrd, Ord)]
pub struct Timestamp {
    /// 48 bits used.
    pub secs: u64,
    pub nanos: u32,
}

impl Timestamp {
    pub fn from_ns(ns: u64) -> Timestamp {
        Timestamp {
            secs: ns / 1_000_000_000,
            nanos: (ns % 1_000_000_000) as u32,
        }
    }
    pub fn to_ns(self) -> u64 {
        self.secs * 1_000_000_000 + u64::from(self.nanos)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub domain: u8,
    pub flags: u16,
    /// Nanoseconds x 2^16 (signed).
    pub correction: i64,
    pub source: PortIdentity,
    pub sequence: u16,
    pub log_interval: i8,
}

impl Header {
    pub fn new(domain: u8, source: PortIdentity, sequence: u16, log_interval: i8) -> Header {
        Header {
            domain,
            flags: 0,
            correction: 0,
            source,
            sequence,
            log_interval,
        }
    }
    /// The correction in nanoseconds (fraction dropped).
    pub fn correction_ns(&self) -> i64 {
        self.correction >> 16
    }
    pub fn two_step(&self) -> bool {
        self.flags & FLAG_TWO_STEP != 0
    }
}

/// What an Announce says about its grandmaster (for choosing the best).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Announce {
    pub origin: Timestamp,
    pub utc_offset: i16,
    pub gm_priority1: u8,
    pub gm_class: u8,
    pub gm_accuracy: u8,
    pub gm_variance: u16,
    pub gm_priority2: u8,
    pub gm_identity: [u8; 8],
    pub steps_removed: u16,
    pub time_source: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    Sync {
        h: Header,
        origin: Timestamp,
    },
    DelayReq {
        h: Header,
        origin: Timestamp,
    },
    FollowUp {
        h: Header,
        precise: Timestamp,
    },
    DelayResp {
        h: Header,
        receive: Timestamp,
        requesting: PortIdentity,
    },
    Announce {
        h: Header,
        a: Announce,
    },
}

impl Message {
    pub fn header(&self) -> &Header {
        match self {
            Message::Sync { h, .. }
            | Message::DelayReq { h, .. }
            | Message::FollowUp { h, .. }
            | Message::DelayResp { h, .. }
            | Message::Announce { h, .. } => h,
        }
    }
    fn kind(&self) -> (u8, u8, usize) {
        // (messageType, controlField, length)
        match self {
            Message::Sync { .. } => (0x0, 0, 44),
            Message::DelayReq { .. } => (0x1, 1, 44),
            Message::FollowUp { .. } => (0x8, 2, 44),
            Message::DelayResp { .. } => (0x9, 3, 54),
            Message::Announce { .. } => (0xB, 5, 64),
        }
    }
}

fn get_ts(b: &[u8]) -> Timestamp {
    let mut s = [0u8; 8];
    s[2..].copy_from_slice(&b[..6]);
    Timestamp {
        secs: u64::from_be_bytes(s),
        nanos: u32::from_be_bytes([b[6], b[7], b[8], b[9]]),
    }
}

fn put_ts(b: &mut [u8], t: Timestamp) {
    b[..6].copy_from_slice(&t.secs.to_be_bytes()[2..]);
    b[6..10].copy_from_slice(&t.nanos.to_be_bytes());
}

fn get_port(b: &[u8]) -> PortIdentity {
    let mut clock = [0u8; 8];
    clock.copy_from_slice(&b[..8]);
    PortIdentity {
        clock,
        port: u16::from_be_bytes([b[8], b[9]]),
    }
}

fn put_port(b: &mut [u8], p: PortIdentity) {
    b[..8].copy_from_slice(&p.clock);
    b[8..10].copy_from_slice(&p.port.to_be_bytes());
}

pub fn parse(b: &[u8]) -> Result<Message, Error> {
    if b.len() < HEADER_LEN {
        return Err(Error::Short);
    }
    if b[1] & 0x0F != 2 {
        return Err(Error::Version);
    }
    let kind = b[0] & 0x0F;
    let h = Header {
        domain: b[4],
        flags: u16::from_be_bytes([b[6], b[7]]),
        correction: i64::from_be_bytes([b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]),
        source: get_port(&b[20..30]),
        sequence: u16::from_be_bytes([b[30], b[31]]),
        log_interval: b[33] as i8,
    };
    let body = &b[HEADER_LEN..];
    let need = |n: usize| {
        if body.len() < n {
            Err(Error::Short)
        } else {
            Ok(())
        }
    };
    Ok(match kind {
        0x0 => {
            need(10)?;
            Message::Sync {
                h,
                origin: get_ts(body),
            }
        }
        0x1 => {
            need(10)?;
            Message::DelayReq {
                h,
                origin: get_ts(body),
            }
        }
        0x8 => {
            need(10)?;
            Message::FollowUp {
                h,
                precise: get_ts(body),
            }
        }
        0x9 => {
            need(20)?;
            Message::DelayResp {
                h,
                receive: get_ts(body),
                requesting: get_port(&body[10..20]),
            }
        }
        0xB => {
            need(30)?;
            Message::Announce {
                h,
                a: Announce {
                    origin: get_ts(body),
                    utc_offset: i16::from_be_bytes([body[10], body[11]]),
                    gm_priority1: body[13],
                    gm_class: body[14],
                    gm_accuracy: body[15],
                    gm_variance: u16::from_be_bytes([body[16], body[17]]),
                    gm_priority2: body[18],
                    gm_identity: {
                        let mut g = [0u8; 8];
                        g.copy_from_slice(&body[19..27]);
                        g
                    },
                    steps_removed: u16::from_be_bytes([body[27], body[28]]),
                    time_source: body[29],
                },
            }
        }
        t => return Err(Error::Unknown(t)),
    })
}

/// Writes `m` into `out`; returns its length.
pub fn write(m: &Message, out: &mut [u8]) -> Result<usize, Error> {
    let (kind, control, len) = m.kind();
    if out.len() < len {
        return Err(Error::Space);
    }
    let b = &mut out[..len];
    b.fill(0);
    let h = m.header();
    b[0] = kind;
    b[1] = 2;
    b[2..4].copy_from_slice(&(len as u16).to_be_bytes());
    b[4] = h.domain;
    b[6..8].copy_from_slice(&h.flags.to_be_bytes());
    b[8..16].copy_from_slice(&h.correction.to_be_bytes());
    put_port(&mut b[20..30], h.source);
    b[30..32].copy_from_slice(&h.sequence.to_be_bytes());
    b[32] = control;
    b[33] = h.log_interval as u8;
    let body = &mut b[HEADER_LEN..];
    match m {
        Message::Sync { origin, .. } | Message::DelayReq { origin, .. } => put_ts(body, *origin),
        Message::FollowUp { precise, .. } => put_ts(body, *precise),
        Message::DelayResp {
            receive,
            requesting,
            ..
        } => {
            put_ts(body, *receive);
            put_port(&mut body[10..20], *requesting);
        }
        Message::Announce { a, .. } => {
            put_ts(body, a.origin);
            body[10..12].copy_from_slice(&a.utc_offset.to_be_bytes());
            body[13] = a.gm_priority1;
            body[14] = a.gm_class;
            body[15] = a.gm_accuracy;
            body[16..18].copy_from_slice(&a.gm_variance.to_be_bytes());
            body[18] = a.gm_priority2;
            body[19..27].copy_from_slice(&a.gm_identity);
            body[27..29].copy_from_slice(&a.steps_removed.to_be_bytes());
            body[29] = a.time_source;
        }
    }
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(n: u8) -> PortIdentity {
        PortIdentity {
            clock: [0xAC, 0xDE, 0x48, 0xFF, 0xFE, 0, 0, n],
            port: 1,
        }
    }

    #[test]
    fn every_message_round_trips() {
        let mut h = Header::new(0, port(1), 4242, -3);
        h.flags = FLAG_TWO_STEP;
        h.correction = -(1_500i64 << 16);
        let ts = Timestamp::from_ns(1_791_600_000_123_456_789);
        let msgs = [
            Message::Sync { h, origin: ts },
            Message::DelayReq { h, origin: ts },
            Message::FollowUp { h, precise: ts },
            Message::DelayResp {
                h,
                receive: ts,
                requesting: port(9),
            },
            Message::Announce {
                h,
                a: Announce {
                    origin: ts,
                    utc_offset: 37,
                    gm_priority1: 128,
                    gm_class: 248,
                    gm_accuracy: 0xFE,
                    gm_variance: 0xFFFF,
                    gm_priority2: 128,
                    gm_identity: port(1).clock,
                    steps_removed: 0,
                    time_source: 0xA0,
                },
            },
        ];
        let mut buf = [0u8; 128];
        for m in msgs {
            let n = write(&m, &mut buf).unwrap_or(0);
            assert_eq!(u16::from_be_bytes([buf[2], buf[3]]) as usize, n);
            assert_eq!(parse(&buf[..n]), Ok(m));
        }
        assert!(h.two_step());
        assert_eq!(h.correction_ns(), -1_500);
        assert_eq!(ts.to_ns(), 1_791_600_000_123_456_789);
    }

    #[test]
    fn parses_a_captured_sync() {
        // A two-step Sync as a typical grandmaster sends it (domain 0,
        // sequence 0x1234, log interval -3, origin 0 s: two-step senders
        // leave it empty).
        let mut b = [0u8; 44];
        b[0] = 0x00;
        b[1] = 0x02;
        b[2..4].copy_from_slice(&44u16.to_be_bytes());
        b[6] = 0x02;
        b[20..28].copy_from_slice(&[0x00, 0x1D, 0xC1, 0xFF, 0xFE, 0x12, 0x34, 0x56]);
        b[29] = 1;
        b[30] = 0x12;
        b[31] = 0x34;
        b[33] = 0xFD;
        match parse(&b) {
            Ok(Message::Sync { h, origin }) => {
                assert!(h.two_step());
                assert_eq!(h.sequence, 0x1234);
                assert_eq!(h.log_interval, -3);
                assert_eq!(origin, Timestamp::default());
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(parse(&b[..20]), Err(Error::Short));
        b[1] = 1;
        assert_eq!(parse(&b), Err(Error::Version));
    }
}
