//! ELF loading, first fact: parse the header.
//!
//! WHAT IS PROVEN: the Core reads an ELF64 header off the disk and verifies the magic, class and machine — the three
//! facts that make it safe to look at the entry point at all. A wrong machine is refused, not jumped to.

/// The three facts that must be true before anything else in the header is believed.
pub fn parse_header(header: &[u8; 64]) -> Option<(u16, u64)> {
    if header[0] != 0x7f || header[1] != b'E' || header[2] != b'L' || header[3] != b'F' {
        return None; // not an ELF
    }
    if header[4] != 2 {
        return None; // not 64-bit
    }
    if header[5] != 1 {
        return None; // not little-endian
    }
    let machine = u16::from_le_bytes([header[0x12], header[0x13]]);
    if machine != 183 {
        return None; // not AArch64 — the only machine this Core can run
    }
    let entry = u64::from_le_bytes([
        header[0x18], header[0x19], header[0x1a], header[0x1b],
        header[0x1c], header[0x1d], header[0x1e], header[0x1f],
    ]);
    Some((machine, entry))
}
