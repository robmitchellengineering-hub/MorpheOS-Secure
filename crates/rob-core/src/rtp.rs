//! RTP header (RFC 3550) plus the ROB `rob-core` header extension, carried in
//! the RFC 8285 one-byte extension form.
//!
//! Spec: "Packet format" in `docs/design/04-protocol-spec-v0.1.md`.
//!
//! `rob-core` element data (9 bytes, big-endian):
//!
//! | byte | content |
//! | --- | --- |
//! | 0 | version (high 4 bits), mode flags (low 4 bits) |
//! | 1 | path_id (high 4 bits), slot bits 11..8 (low 4 bits) |
//! | 2 | slot bits 7..0 |
//! | 3..5 | budget_us (u16) |
//! | 5..9 | send_time_us (u32, low 32 bits of network time in µs) |

use crate::Error;

/// RTP version carried in every packet.
pub const RTP_VERSION: u8 = 2;
/// Fixed RTP header length (no CSRCs).
pub const RTP_HEADER_LEN: usize = 12;
/// RFC 8285 one-byte-header profile value.
pub const ONE_BYTE_PROFILE: u16 = 0xBEDE;
/// Data length of the `rob-core` element.
pub const ROB_CORE_DATA_LEN: usize = 9;
/// Whole extension block on the wire: 4-byte RFC 8285 header, 1-byte element
/// header, 9 data bytes, 2 bytes of padding.
pub const ROB_EXT_LEN: usize = 16;
/// Draft version written into `rob-core`.
pub const ROB_VERSION: u8 = 0;
/// Default extension element ID. The real ID is negotiated in SDP (`a=extmap`).
pub const DEFAULT_ROB_EXT_ID: u8 = 1;

/// `mode` flag: bit-exact payload (no layering).
pub const MODE_BIT_EXACT: u8 = 0x1;
/// `mode` flag: redundancy blocks for earlier periods are present.
pub const MODE_REDUNDANCY: u8 = 0x2;
/// `mode` flag: payload uses lossless compression.
pub const MODE_COMPRESSED: u8 = 0x4;
/// `mode` flag: packet carries a test signal.
pub const MODE_TEST: u8 = 0x8;

/// The fixed RTP header fields ROB uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RtpHeader {
    pub marker: bool,
    pub payload_type: u8,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
}

/// The `rob-core` header extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RobCore {
    /// Protocol draft version (4 bits).
    pub version: u8,
    /// `MODE_*` flags (4 bits).
    pub mode: u8,
    /// Which path sent this copy (4 bits).
    pub path_id: u8,
    /// Soft time slot index (12 bits).
    pub slot: u16,
    /// Microseconds left before the playout deadline, at send time.
    pub budget_us: u16,
    /// Sender network time at transmit, µs, low 32 bits.
    pub send_time_us: u32,
}

impl RobCore {
    fn validate(&self) -> Result<(), Error> {
        if self.version > 0xF || self.mode > 0xF || self.path_id > 0xF || self.slot > 0xFFF {
            return Err(Error::FieldRange);
        }
        Ok(())
    }

    fn encode(&self) -> [u8; ROB_CORE_DATA_LEN] {
        let b = self.budget_us.to_be_bytes();
        let t = self.send_time_us.to_be_bytes();
        [
            (self.version << 4) | self.mode,
            (self.path_id << 4) | ((self.slot >> 8) as u8 & 0x0F),
            (self.slot & 0xFF) as u8,
            b[0],
            b[1],
            t[0],
            t[1],
            t[2],
            t[3],
        ]
    }

    fn decode(d: &[u8]) -> Result<Self, Error> {
        if d.len() < ROB_CORE_DATA_LEN {
            return Err(Error::BadExtension);
        }
        Ok(RobCore {
            version: d[0] >> 4,
            mode: d[0] & 0x0F,
            path_id: d[1] >> 4,
            slot: (u16::from(d[1] & 0x0F) << 8) | u16::from(d[2]),
            budget_us: u16::from_be_bytes([d[3], d[4]]),
            send_time_us: u32::from_be_bytes([d[5], d[6], d[7], d[8]]),
        })
    }
}

/// Writes the RTP header and, if given, the `rob-core` extension into `out`.
/// Returns the number of bytes written (12, or 28 with the extension).
pub fn write_header(
    out: &mut [u8],
    h: &RtpHeader,
    ext: Option<(u8, &RobCore)>,
) -> Result<usize, Error> {
    let needed = RTP_HEADER_LEN + if ext.is_some() { ROB_EXT_LEN } else { 0 };
    if out.len() < needed {
        return Err(Error::OutputTooSmall);
    }
    if h.payload_type > 0x7F {
        return Err(Error::FieldRange);
    }
    let x_bit = if ext.is_some() { 0x10 } else { 0 };
    out[0] = (RTP_VERSION << 6) | x_bit;
    out[1] = (u8::from(h.marker) << 7) | h.payload_type;
    out[2..4].copy_from_slice(&h.sequence.to_be_bytes());
    out[4..8].copy_from_slice(&h.timestamp.to_be_bytes());
    out[8..12].copy_from_slice(&h.ssrc.to_be_bytes());
    if let Some((id, core)) = ext {
        if id == 0 || id >= 15 {
            return Err(Error::FieldRange);
        }
        core.validate()?;
        let e = &mut out[RTP_HEADER_LEN..RTP_HEADER_LEN + ROB_EXT_LEN];
        e[0..2].copy_from_slice(&ONE_BYTE_PROFILE.to_be_bytes());
        // Extension length in 32-bit words, excluding the 4-byte header.
        e[2..4].copy_from_slice(&(((ROB_EXT_LEN - 4) / 4) as u16).to_be_bytes());
        e[4] = (id << 4) | (ROB_CORE_DATA_LEN as u8 - 1);
        e[5..5 + ROB_CORE_DATA_LEN].copy_from_slice(&core.encode());
        e[5 + ROB_CORE_DATA_LEN..].fill(0);
    }
    Ok(needed)
}

/// A parsed ROB packet, borrowing its payload from the input buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Packet<'a> {
    pub header: RtpHeader,
    /// `None` for plain RTP (for example AES67 interop flows).
    pub rob: Option<RobCore>,
    pub payload: &'a [u8],
}

/// Parses an RTP packet, looking for the `rob-core` element with `rob_ext_id`.
/// Unknown extension elements are skipped, as RFC 8285 requires.
pub fn parse(buf: &[u8], rob_ext_id: u8) -> Result<Packet<'_>, Error> {
    if buf.len() < RTP_HEADER_LEN {
        return Err(Error::TooShort);
    }
    if buf[0] >> 6 != RTP_VERSION {
        return Err(Error::BadVersion);
    }
    if buf[0] & 0x0F != 0 {
        return Err(Error::CsrcUnsupported);
    }
    let padding = buf[0] & 0x20 != 0;
    let has_ext = buf[0] & 0x10 != 0;
    let header = RtpHeader {
        marker: buf[1] & 0x80 != 0,
        payload_type: buf[1] & 0x7F,
        sequence: u16::from_be_bytes([buf[2], buf[3]]),
        timestamp: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
        ssrc: u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]),
    };

    let mut end = buf.len();
    if padding {
        let pad = usize::from(buf[end - 1]);
        if pad == 0 || pad > end - RTP_HEADER_LEN {
            return Err(Error::TooShort);
        }
        end -= pad;
    }

    let mut pos = RTP_HEADER_LEN;
    let mut rob = None;
    if has_ext {
        if end < pos + 4 {
            return Err(Error::TooShort);
        }
        let profile = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let words = usize::from(u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]));
        let ext_end = pos + 4 + words * 4;
        if ext_end > end {
            return Err(Error::TooShort);
        }
        if profile == ONE_BYTE_PROFILE {
            let mut p = pos + 4;
            while p < ext_end {
                let b = buf[p];
                if b == 0 {
                    p += 1; // padding byte
                    continue;
                }
                let id = b >> 4;
                if id == 15 {
                    break; // reserved: stop parsing, per RFC 8285
                }
                let len = usize::from(b & 0x0F) + 1;
                let data_start = p + 1;
                let data_end = data_start + len;
                if data_end > ext_end {
                    return Err(Error::BadExtension);
                }
                if id == rob_ext_id {
                    rob = Some(RobCore::decode(&buf[data_start..data_end])?);
                }
                p = data_end;
            }
        }
        pos = ext_end;
    }

    Ok(Packet {
        header,
        rob,
        payload: &buf[pos..end],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (RtpHeader, RobCore) {
        (
            RtpHeader {
                marker: true,
                payload_type: 97,
                sequence: 0xFFFE,
                timestamp: 0xDEAD_BEEF,
                ssrc: 0x1234_5678,
            },
            RobCore {
                version: ROB_VERSION,
                mode: MODE_REDUNDANCY | MODE_TEST,
                path_id: 3,
                slot: 0xABC,
                budget_us: 7_500,
                send_time_us: 0x0102_0304,
            },
        )
    }

    #[test]
    fn header_with_extension_round_trips() {
        let (h, c) = sample();
        let mut buf = [0u8; 64];
        let n = write_header(&mut buf, &h, Some((DEFAULT_ROB_EXT_ID, &c))).unwrap();
        assert_eq!(n, 28, "spec: RTP + rob-core = 28 bytes");
        buf[n..n + 4].copy_from_slice(b"DATA");
        let p = parse(&buf[..n + 4], DEFAULT_ROB_EXT_ID).unwrap();
        assert_eq!(p.header, h);
        assert_eq!(p.rob, Some(c));
        assert_eq!(p.payload, b"DATA");
    }

    #[test]
    fn plain_rtp_has_no_rob_fields() {
        let (h, _) = sample();
        let mut buf = [0u8; 16];
        let n = write_header(&mut buf, &h, None).unwrap();
        assert_eq!(n, 12);
        let p = parse(&buf[..n], DEFAULT_ROB_EXT_ID).unwrap();
        assert_eq!(p.rob, None);
        assert!(p.payload.is_empty());
    }

    #[test]
    fn other_extension_ids_are_skipped() {
        let (h, c) = sample();
        let mut buf = [0u8; 64];
        let n = write_header(&mut buf, &h, Some((5, &c))).unwrap();
        let p = parse(&buf[..n], DEFAULT_ROB_EXT_ID).unwrap();
        assert_eq!(p.rob, None, "element 5 is not rob-core when we asked for 1");
    }

    #[test]
    fn rejects_out_of_range_fields() {
        let (h, mut c) = sample();
        c.slot = 0x1000;
        let mut buf = [0u8; 64];
        assert_eq!(
            write_header(&mut buf, &h, Some((1, &c))),
            Err(Error::FieldRange)
        );
    }

    #[test]
    fn rejects_truncated_and_wrong_version() {
        assert_eq!(parse(&[0x80; 5], 1), Err(Error::TooShort));
        let mut buf = [0u8; 12];
        buf[0] = 0x40; // version 1
        assert_eq!(parse(&buf, 1), Err(Error::BadVersion));
    }

    #[test]
    fn rtp_padding_is_removed() {
        let (h, _) = sample();
        let mut buf = [0u8; 20];
        let n = write_header(&mut buf, &h, None).unwrap();
        buf[0] |= 0x20;
        buf[n..n + 2].copy_from_slice(b"AB");
        buf[n + 2..n + 4].copy_from_slice(&[0, 2]); // 2 bytes of padding
        let p = parse(&buf[..n + 4], 1).unwrap();
        assert_eq!(p.payload, b"AB");
    }
}
