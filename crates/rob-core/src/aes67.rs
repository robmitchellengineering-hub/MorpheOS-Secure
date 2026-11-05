//! AES67 interop formats: the stream description (SDP), its announcement
//! (SAP, RFC 2974, as Dante in AES67 mode uses), and the L24 / L16 audio
//! payload (big-endian, interleaved).
//!
//! An AES67 stream's RTP timestamp is its PTP time in samples plus the
//! `a=mediaclk:direct=` offset (modulo 2^32). ROB network time is PTP time
//! when following PTP, so `timestamp - offset` is ROB's own sample index.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as _;
use core::net::Ipv4Addr;
use core::write;
use core::writeln;

/// SAP's group for administratively scoped (239.x) sessions, and its port.
pub const SAP_GROUP: [u8; 4] = [239, 255, 255, 255];
pub const SAP_PORT: u16 = 9875;
const SAP_MIME: &[u8] = b"application/sdp\0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Missing(&'static str),
    Bad(&'static str),
    NotSdp,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Missing(w) => write!(f, "SDP has no {w}"),
            Error::Bad(w) => write!(f, "SDP has a bad {w}"),
            Error::NotSdp => write!(f, "not a SAP announcement carrying SDP"),
        }
    }
}

impl core::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    L24,
    L16,
}

impl Encoding {
    pub fn bytes(self) -> usize {
        match self {
            Encoding::L24 => 3,
            Encoding::L16 => 2,
        }
    }
}

/// One AES67 stream as an SDP describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub name: String,
    pub session_id: u64,
    pub origin: Ipv4Addr,
    /// Destination (multicast group or unicast address) and port.
    pub dest: Ipv4Addr,
    pub port: u16,
    pub payload_type: u8,
    pub encoding: Encoding,
    pub sample_rate: u32,
    pub channels: usize,
    /// Packet time (ms): 1 is the AES67 default; 0.125 .. 4 occur.
    pub ptime_ms: f64,
    /// PTP grandmaster and domain the stream is timed by, if given.
    pub ptp_gm: Option<[u8; 8]>,
    pub ptp_domain: Option<u8>,
    /// RTP timestamp minus PTP time in samples.
    pub media_offset: u32,
    /// Only packets from this sender (source-specific multicast), if given.
    pub source: Option<Ipv4Addr>,
}

impl Stream {
    /// Frames per packet.
    pub fn frames(&self) -> usize {
        libm::round(self.ptime_ms * f64::from(self.sample_rate) / 1_000.0) as usize
    }

    /// Parses an SDP (one audio stream: the first `m=audio`).
    pub fn from_sdp(sdp: &str) -> Result<Stream, Error> {
        let mut s = Stream {
            name: String::new(),
            session_id: 0,
            origin: Ipv4Addr::UNSPECIFIED,
            dest: Ipv4Addr::UNSPECIFIED,
            port: 0,
            payload_type: 0,
            encoding: Encoding::L24,
            sample_rate: 0,
            channels: 0,
            ptime_ms: 1.0,
            ptp_gm: None,
            ptp_domain: None,
            media_offset: 0,
            source: None,
        };
        let mut have_m = false;
        let mut have_map = false;
        for line in sdp.lines().map(str::trim) {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            match k {
                "s" => s.name = v.to_string(),
                "o" => {
                    let f: Vec<&str> = v.split_whitespace().collect();
                    if f.len() >= 6 {
                        s.session_id = f[1].parse().unwrap_or(0);
                        s.origin = f[5].parse().map_err(|_| Error::Bad("o= address"))?;
                    }
                }
                "c" => {
                    let addr = v.split_whitespace().nth(2).ok_or(Error::Bad("c= line"))?;
                    let ip = addr.split('/').next().unwrap_or(addr);
                    s.dest = ip.parse().map_err(|_| Error::Bad("c= address"))?;
                }
                "m" if !have_m => {
                    let f: Vec<&str> = v.split_whitespace().collect();
                    if f.first() != Some(&"audio") || f.len() < 4 {
                        continue;
                    }
                    s.port = f[1]
                        .split('/')
                        .next()
                        .unwrap_or("")
                        .parse()
                        .map_err(|_| Error::Bad("m= port"))?;
                    s.payload_type = f[3].parse().map_err(|_| Error::Bad("m= payload type"))?;
                    have_m = true;
                }
                "a" => {
                    let (name, val) = v.split_once(':').unwrap_or((v, ""));
                    match name {
                        "rtpmap" => {
                            let (pt, enc) = val.split_once(' ').ok_or(Error::Bad("rtpmap"))?;
                            if pt.parse::<u8>().ok() != Some(s.payload_type) && have_m {
                                continue;
                            }
                            let f: Vec<&str> = enc.split('/').collect();
                            s.encoding = match f.first().map(|x| x.to_ascii_uppercase()).as_deref()
                            {
                                Some("L24") => Encoding::L24,
                                Some("L16") => Encoding::L16,
                                _ => return Err(Error::Bad("encoding (L24 or L16 only)")),
                            };
                            s.sample_rate = f
                                .get(1)
                                .and_then(|x| x.parse().ok())
                                .ok_or(Error::Bad("rtpmap rate"))?;
                            s.channels = f.get(2).and_then(|x| x.parse().ok()).unwrap_or(1);
                            have_map = true;
                        }
                        "ptime" => {
                            s.ptime_ms = val.trim().parse().map_err(|_| Error::Bad("ptime"))?
                        }
                        "ts-refclk" => {
                            // ptp=IEEE1588-2008:00-1D-C1-FF-FE-12-34-56:0
                            if let Some(rest) = val.strip_prefix("ptp=IEEE1588-2008:") {
                                let mut parts = rest.split(':');
                                if let Some(gm) = parts.next() {
                                    let b: Vec<u8> = gm
                                        .split('-')
                                        .filter_map(|x| u8::from_str_radix(x, 16).ok())
                                        .collect();
                                    if b.len() == 8 {
                                        let mut g = [0u8; 8];
                                        g.copy_from_slice(&b);
                                        s.ptp_gm = Some(g);
                                    }
                                }
                                s.ptp_domain = parts.next().and_then(|d| d.trim().parse().ok());
                            }
                        }
                        "mediaclk" => {
                            if let Some(o) = val.strip_prefix("direct=") {
                                let o = o.split_whitespace().next().unwrap_or("0");
                                s.media_offset =
                                    o.parse().map_err(|_| Error::Bad("mediaclk offset"))?;
                            }
                        }
                        "source-filter" => {
                            // incl IN IP4 <dest> <source>
                            let f: Vec<&str> = val.split_whitespace().collect();
                            if f.len() >= 5 {
                                s.source = f[4].parse().ok();
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        if !have_m {
            return Err(Error::Missing("m=audio line"));
        }
        if !have_map {
            return Err(Error::Missing("a=rtpmap"));
        }
        if s.dest.is_unspecified() {
            return Err(Error::Missing("c= address"));
        }
        if s.channels == 0 || s.channels > 64 {
            return Err(Error::Bad("channel count"));
        }
        Ok(s)
    }

    /// Writes this stream as an SDP.
    pub fn to_sdp(&self) -> String {
        let mut o = String::new();
        let _ = writeln!(o, "v=0");
        let _ = writeln!(
            o,
            "o=- {} {} IN IP4 {}",
            self.session_id, self.session_id, self.origin
        );
        let _ = writeln!(o, "s={}", self.name);
        let ttl = if self.dest.is_multicast() { "/32" } else { "" };
        let _ = writeln!(o, "c=IN IP4 {}{ttl}", self.dest);
        let _ = writeln!(o, "t=0 0");
        let _ = writeln!(o, "m=audio {} RTP/AVP {}", self.port, self.payload_type);
        let enc = match self.encoding {
            Encoding::L24 => "L24",
            Encoding::L16 => "L16",
        };
        let _ = writeln!(
            o,
            "a=rtpmap:{} {enc}/{}/{}",
            self.payload_type, self.sample_rate, self.channels
        );
        let _ = writeln!(o, "a=ptime:{}", self.ptime_ms);
        if let Some(g) = self.ptp_gm {
            let gm: Vec<String> = g.iter().map(|b| format!("{b:02X}")).collect();
            let _ = writeln!(
                o,
                "a=ts-refclk:ptp=IEEE1588-2008:{}:{}",
                gm.join("-"),
                self.ptp_domain.unwrap_or(0)
            );
        }
        let _ = writeln!(o, "a=mediaclk:direct={}", self.media_offset);
        let _ = writeln!(o, "a=recvonly");
        o
    }
}

/// A SAP packet: (deletion?, originating source, SDP text).
pub fn parse_sap(b: &[u8]) -> Result<(bool, Ipv4Addr, &str), Error> {
    if b.len() < 8 || b[0] >> 5 != 1 || b[0] & 0x10 != 0 {
        // Version 1, IPv4 origin only.
        return Err(Error::NotSdp);
    }
    let delete = b[0] & 0x04 != 0;
    if b[0] & 0x03 != 0 {
        return Err(Error::NotSdp); // encrypted or compressed
    }
    let auth = usize::from(b[1]) * 4;
    let origin = Ipv4Addr::new(b[4], b[5], b[6], b[7]);
    let mut rest = b.get(8 + auth..).ok_or(Error::NotSdp)?;
    if !rest.starts_with(b"v=0") {
        // An optional payload type ("application/sdp\0").
        let nul = rest.iter().position(|&c| c == 0).ok_or(Error::NotSdp)?;
        if &rest[..=nul] != SAP_MIME {
            return Err(Error::NotSdp);
        }
        rest = &rest[nul + 1..];
    }
    let text = core::str::from_utf8(rest).map_err(|_| Error::NotSdp)?;
    Ok((delete, origin, text))
}

/// Writes a SAP announcement (or deletion) of `sdp` from `origin`.
pub fn write_sap(sdp: &str, origin: Ipv4Addr, hash: u16, delete: bool, out: &mut Vec<u8>) {
    out.clear();
    out.push(0x20 | if delete { 0x04 } else { 0 });
    out.push(0);
    out.extend_from_slice(&hash.to_be_bytes());
    out.extend_from_slice(&origin.octets());
    out.extend_from_slice(SAP_MIME);
    out.extend_from_slice(sdp.as_bytes());
}

/// Decodes one channel of an interleaved L24/L16 payload into 24-bit
/// integers. Returns frames decoded. No allocation.
pub fn decode_channel(
    payload: &[u8],
    enc: Encoding,
    channels: usize,
    ch: usize,
    out: &mut [i32],
) -> usize {
    let w = enc.bytes();
    let frame = w * channels;
    if channels == 0 || ch >= channels {
        return 0;
    }
    let frames = (payload.len() / frame).min(out.len());
    for (k, o) in out.iter_mut().enumerate().take(frames) {
        let p = &payload[k * frame + ch * w..];
        *o = match enc {
            Encoding::L24 => (i32::from_be_bytes([p[0], p[1], p[2], 0])) >> 8,
            Encoding::L16 => i32::from(i16::from_be_bytes([p[0], p[1]])) << 8,
        };
    }
    frames
}

/// Encodes interleaved 24-bit integers as L24. Returns bytes written.
pub fn encode_l24(samples: &[i32], out: &mut [u8]) -> usize {
    let n = samples.len().min(out.len() / 3);
    for (k, &s) in samples.iter().enumerate().take(n) {
        let b = (s.clamp(-8_388_608, 8_388_607)).to_be_bytes();
        out[3 * k..3 * k + 3].copy_from_slice(&b[1..4]);
    }
    3 * n
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As a Dante device in AES67 mode announces a stream.
    const DANTE_SDP: &str = "v=0\r\n\
o=- 1311738121 1311738121 IN IP4 192.168.1.100\r\n\
s=Stagebox-AES67 : 2\r\n\
c=IN IP4 239.69.83.67/32\r\n\
t=0 0\r\n\
a=keywds:Dante\r\n\
m=audio 5004 RTP/AVP 97\r\n\
i=8 channels: 01, 02, 03, 04, 05, 06, 07, 08\r\n\
a=recvonly\r\n\
a=rtpmap:97 L24/48000/8\r\n\
a=ptime:1\r\n\
a=ts-refclk:ptp=IEEE1588-2008:00-1D-C1-FF-FE-12-34-56:0\r\n\
a=mediaclk:direct=1234567\r\n";

    #[test]
    fn parses_a_dante_style_sdp() {
        let s = Stream::from_sdp(DANTE_SDP).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.name, "Stagebox-AES67 : 2");
        assert_eq!(s.dest, Ipv4Addr::new(239, 69, 83, 67));
        assert_eq!(s.port, 5004);
        assert_eq!(s.payload_type, 97);
        assert_eq!(s.encoding, Encoding::L24);
        assert_eq!((s.sample_rate, s.channels), (48_000, 8));
        assert_eq!(s.frames(), 48);
        assert_eq!(
            s.ptp_gm,
            Some([0x00, 0x1D, 0xC1, 0xFF, 0xFE, 0x12, 0x34, 0x56])
        );
        assert_eq!(s.ptp_domain, Some(0));
        assert_eq!(s.media_offset, 1_234_567);
        assert_eq!(s.origin, Ipv4Addr::new(192, 168, 1, 100));
        // Our own SDP parses back the same.
        let again = Stream::from_sdp(&s.to_sdp()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(again, s);
    }

    #[test]
    fn rejects_what_it_cannot_play() {
        assert_eq!(
            Stream::from_sdp("v=0\nc=IN IP4 239.1.1.1\n"),
            Err(Error::Missing("m=audio line"))
        );
        let opus = DANTE_SDP.replace("L24/48000/8", "opus/48000/2");
        assert!(Stream::from_sdp(&opus).is_err());
    }

    #[test]
    fn sap_round_trip_and_bare_sdp() {
        let mut b = Vec::new();
        write_sap(
            DANTE_SDP,
            Ipv4Addr::new(192, 168, 1, 100),
            0xBEEF,
            false,
            &mut b,
        );
        let (del, origin, sdp) = parse_sap(&b).unwrap_or_else(|e| panic!("{e}"));
        assert!(!del);
        assert_eq!(origin, Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(sdp, DANTE_SDP);
        // Some senders leave out the payload type string.
        let mut bare = b[..8].to_vec();
        bare.extend_from_slice(DANTE_SDP.as_bytes());
        assert_eq!(parse_sap(&bare).map(|x| x.2), Ok(DANTE_SDP));
        write_sap(DANTE_SDP, Ipv4Addr::LOCALHOST, 1, true, &mut b);
        assert!(parse_sap(&b).map(|x| x.0).unwrap_or(false), "deletion flag");
    }

    #[test]
    fn l24_and_l16_round_trip() {
        let src = [0, 1, -1, 8_388_607, -8_388_608, 123_456, -654_321, 42];
        let mut bytes = [0u8; 24];
        assert_eq!(encode_l24(&src, &mut bytes), 24);
        // As 4 frames of 2 channels.
        let (mut l, mut r) = ([0i32; 4], [0i32; 4]);
        assert_eq!(decode_channel(&bytes, Encoding::L24, 2, 0, &mut l), 4);
        decode_channel(&bytes, Encoding::L24, 2, 1, &mut r);
        assert_eq!(l, [0, -1, -8_388_608, -654_321]);
        assert_eq!(r, [1, 8_388_607, 123_456, 42]);
        let l16 = [0x7F, 0xFF, 0x80, 0x00];
        let mut o = [0i32; 2];
        decode_channel(&l16, Encoding::L16, 1, 0, &mut o);
        assert_eq!(o, [32_767 << 8, -32_768 << 8]);
    }
}
