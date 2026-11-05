//! Archive retransmission (design 02, idea 6): the recorder asks a node to
//! resend one packet by its RTP sequence number, and the node answers from its
//! rolling buffer. A separate, deadline-free channel from the live audio path
//! (the protocol spec's D8 "archive path").

use crate::Error;

pub const REQ_MAGIC: [u8; 4] = *b"RRSQ";
pub const RESP_MAGIC: [u8; 4] = *b"RRSP";
pub const REQ_LEN: usize = 6;
/// The UDP port a node answers archive resend requests on (design 02 idea 6).
/// The recorder learns a node's address from the audio it hears and sends its
/// request to this well-known port on that address. **5008, not 5007**: the
/// field kit's sync port is 5007 (`base+3`), and binding the resend server
/// there made `rob-io send --serve-sync` exit (address in use).
pub const RESEND_PORT: u16 = 5008;

/// Writes a resend request: `"RRSQ" seq`. Returns bytes written.
pub fn write_req(seq: u16, out: &mut [u8]) -> Result<usize, Error> {
    if out.len() < REQ_LEN {
        return Err(Error::OutputTooSmall);
    }
    out[0..4].copy_from_slice(&REQ_MAGIC);
    out[4..6].copy_from_slice(&seq.to_le_bytes());
    Ok(REQ_LEN)
}

/// Parses a request, or `None` if the datagram is not one.
pub fn parse_req(buf: &[u8]) -> Option<u16> {
    if buf.len() < REQ_LEN || buf[0..4] != REQ_MAGIC {
        return None;
    }
    Some(u16::from_le_bytes([buf[4], buf[5]]))
}

/// Writes a response: `"RRSP" seq len packet`. `packet` empty = not found.
/// Returns bytes written.
pub fn write_resp(seq: u16, packet: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    let n = 8 + packet.len();
    if out.len() < n {
        return Err(Error::OutputTooSmall);
    }
    out[0..4].copy_from_slice(&RESP_MAGIC);
    out[4..6].copy_from_slice(&seq.to_le_bytes());
    out[6..8].copy_from_slice(&(packet.len() as u16).to_le_bytes());
    out[8..8 + packet.len()].copy_from_slice(packet);
    Ok(n)
}

/// Parses a response: `(seq, packet)`. An empty packet means "not found".
pub fn parse_resp(buf: &[u8]) -> Option<(u16, &[u8])> {
    if buf.len() < 8 || buf[0..4] != RESP_MAGIC {
        return None;
    }
    let seq = u16::from_le_bytes([buf[4], buf[5]]);
    let len = u16::from_le_bytes([buf[6], buf[7]]) as usize;
    if buf.len() < 8 + len {
        return None;
    }
    Some((seq, &buf[8..8 + len]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_and_response_round_trip() {
        let mut req = [0u8; 16];
        let n = write_req(42, &mut req).unwrap();
        assert_eq!(n, REQ_LEN);
        assert_eq!(parse_req(&req[..n]), Some(42));

        let mut resp = [0u8; 64];
        let n = write_resp(42, b"packet", &mut resp).unwrap();
        assert_eq!(n, 8 + 6);
        assert_eq!(parse_resp(&resp[..n]), Some((42, &b"packet"[..])));
    }

    #[test]
    fn a_not_found_response_has_no_packet() {
        let mut resp = [0u8; 16];
        let n = write_resp(7, &[], &mut resp).unwrap();
        assert_eq!(n, 8);
        assert_eq!(parse_resp(&resp[..n]), Some((7, &[][..])));
    }

    #[test]
    fn foreign_and_truncated_are_refused() {
        assert_eq!(parse_req(b"not a request"), None);
        assert_eq!(parse_resp(b"not a response"), None);
        assert_eq!(parse_resp(&[0u8; 6]), None);
    }
}
