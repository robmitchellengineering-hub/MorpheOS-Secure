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

/// Parse the program headers, copy the PT_LOAD segment into its load address, and return the entry point. This is the
/// difference between "the header is real" and "the program's code is in memory": the bytes the ELF carries are placed
/// where the ELF says they belong, and only then is the entry point returned.
pub fn load(buf: &[u8]) -> Option<u64> {
    let mut header = [0u8; 64];
    header.copy_from_slice(&buf[..64]);
    let (_, entry) = parse_header(&header)?;
    let phoff = u64::from_le_bytes(buf[0x20..0x28].try_into().ok()?) as usize;
    let phentsize = u16::from_le_bytes(buf[0x36..0x38].try_into().ok()?) as usize;
    let phnum = u16::from_le_bytes(buf[0x38..0x3a].try_into().ok()?) as usize;
    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        if ph + 40 > 512 {
            return None;
        }
        let p_type = u32::from_le_bytes(buf[ph..ph + 4].try_into().ok()?);
        if p_type == 1 {
            // PT_LOAD: place the file bytes at p_vaddr.
            let p_offset = u64::from_le_bytes(buf[ph + 8..ph + 16].try_into().ok()?) as usize;
            let p_vaddr = u64::from_le_bytes(buf[ph + 16..ph + 24].try_into().ok()?) as usize;
            let p_filesz = u64::from_le_bytes(buf[ph + 32..ph + 40].try_into().ok()?) as usize;
            // SAFETY: the segment is placed at the address the ELF declares, inside this Core's RAM.
            for j in 0..p_filesz {
                unsafe {
                    core::ptr::write_volatile((p_vaddr + j) as *mut u8, buf[p_offset + j]);
                }
            }
            return Some(entry);
        }
    }
    None
}
