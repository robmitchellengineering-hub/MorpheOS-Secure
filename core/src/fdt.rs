//! The device tree reader — the one genuinely new piece for the Pi, and it can be proven on QEMU first.
//!
//! WHY THIS EXISTS: on the Pi, every address this Core needs — the UART, RAM's base and size, the interrupt controller
//! — is in the firmware's device tree. This project has already paid six rounds for believing a remembered GIC
//! version over the machine's own word, so the rule is: read the tree, do not recite it.
//!
//! WHAT IT IS: a bounded walker over the flattened device tree (FDT). No allocation, a fixed node budget, and a
//! magic/total-size check before any pointer is followed — a malformed tree is REFUSED, not walked.
//!
//! WHAT IT IS NOT: a general FDT library. It answers two questions — "where is the node with this compatible string,
//! and what are its `reg` or `memory` numbers" — because two questions are what this rung needs.

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

/// Stop after this many nodes: a tree that is not the firmware's cannot make this walk unbounded.
const NODE_BUDGET: usize = 4096;

pub struct Answered {
    /// Base address from the node's `reg` (or the memory node's first cell).
    pub base: u64,
    /// Size, where the node states two cells.
    pub size: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// The blob does not start with the FDT magic.
    BadMagic,
    /// The declared size is not sane, so the walk would read outside the blob.
    BadSize,
    /// The walk ran out of budget, or the structure ended before the answer appeared.
    NotFound,
}

fn read_u32(ptr: *const u8, offset: usize) -> u32 {
    // SAFETY: callers have checked the offset against the blob's declared size.
    unsafe { u32::from_be(core::ptr::read_unaligned(ptr.add(offset).cast::<u32>())) }
}

/// Walk the tree looking for a node whose `compatible` contains `wanted`, and return its `reg` cells.
///
/// `blob` is the pointer the firmware passed in `x0`; `total` is the blob's declared total size.
pub fn find_compatible(blob: usize, wanted: &str) -> Result<Answered, Refused> {
    if blob == 0 {
        return Err(Refused::BadMagic);
    }
    let base = blob as *const u8;
    if read_u32(base, 0) != FDT_MAGIC {
        return Err(Refused::BadMagic);
    }
    let total = read_u32(base, 4) as usize;
    if total < 64 || total > 0x20_0000 {
        // 2 MiB is generous for a firmware tree and keeps every offset checked against something real.
        return Err(Refused::BadSize);
    }
    let off_struct = read_u32(base, 8) as usize;
    let off_strings = read_u32(base, 12) as usize;
    if off_struct >= total || off_strings >= total {
        return Err(Refused::BadSize);
    }

    let mut depth_match = false;
    let mut nodes = 0usize;
    let mut cursor = off_struct;
    while cursor + 4 <= total {
        let token = read_u32(base, cursor);
        cursor += 4;
        match token {
            FDT_BEGIN_NODE => {
                nodes += 1;
                if nodes > NODE_BUDGET {
                    return Err(Refused::NotFound);
                }
                // Skip the node's name (NUL-terminated, 4-byte aligned).
                while cursor < total && base.cast::<u8>().is_null() == false {
                    // SAFETY: cursor < total, checked.
                    let byte = unsafe { *base.add(cursor) };
                    cursor += 1;
                    if byte == 0 {
                        break;
                    }
                }
                cursor = (cursor + 3) & !3;
                depth_match = false;
            }
            FDT_END_NODE => {
                depth_match = false;
            }
            FDT_PROP => {
                if cursor + 8 > total {
                    return Err(Refused::NotFound);
                }
                let len = read_u32(base, cursor) as usize;
                let name_off = read_u32(base, cursor + 4) as usize;
                cursor += 8;
                if cursor + len > total || off_strings + name_off >= total {
                    return Err(Refused::NotFound);
                }
                // SAFETY: both offsets were checked against the declared total size.
                let name = unsafe { core::ffi::CStr::from_ptr(base.add(off_strings + name_off).cast()) };
                let name = name.to_bytes();
                if name == b"compatible" && cfg!(not(feature = "fdt-mutant-skip-walk")) {
                    // The value is a list of NUL-terminated strings.
                    let mut start = 0usize;
                    while start < len {
                        // SAFETY: inside the property, which was bounds-checked.
                        let end = unsafe {
                            let mut e = start;
                            while e < len && *base.add(cursor + e) != 0 {
                                e += 1;
                            }
                            e
                        };
                        let value = unsafe { core::slice::from_raw_parts(base.add(cursor + start), end - start) };
                        if value == wanted.as_bytes() {
                            depth_match = true;
                        }
                        start = end + 1;
                    }
                } else if name == b"reg" && depth_match {
                    if len < 8 {
                        return Err(Refused::NotFound);
                    }
                    let hi = read_u32(base, cursor) as u64;
                    let lo = read_u32(base, cursor + 4) as u64;
                    let size = if len >= 16 { read_u32(base, cursor + 12) as u64 } else { 0 };
                    return Ok(Answered { base: (hi << 32) | lo, size });
                }
                cursor += (len + 3) & !3;
            }
            FDT_NOP => {}
            FDT_END => break,
            _ => return Err(Refused::NotFound),
        }
    }
    Err(Refused::NotFound)
}
