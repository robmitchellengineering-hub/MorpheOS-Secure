//! The ledger — rung M2.
//!
//! THE CLAIM THIS FILE EXISTS TO MAKE: the machine **is its history**. State is a chain of signed generations, not a
//! pile of mutable bytes: every change appends a generation whose parent is the machine's previous state, reading
//! returns the current generation's payload, and "undo" walks back down the chain without a reboot. A generation that
//! does not check out is refused rather than used.
//!
//! WHAT "SIGNED" MEANS HERE, HONESTLY: on this rung it is a **checksum we compute ourselves** (FNV-1a over the
//! generation's fields), not a cryptographic signature. That catches a corrupted or edited generation, which is what
//! the falsifier attacks; it does NOT catch an attacker who can rewrite the payload *and* recompute the checksum.
//! A real signature (Ed25519 or similar, with a key the machine holds) is M2b, and it needs the arithmetic and the
//! key storage that M4's seal brings. Saying this now is the difference between a ledger and a story about one.
//!
//! WHAT IT IS FOR, in the product: a show's mix moves and scene changes become generations, so "undo the last cue"
//! is a property of the machine rather than a feature somebody remembered to add to a console.

use crate::uart::Uart;

/// A generation of the machine's state. `parent` is the id of the generation this one supersedes — a chain, not a list.
#[derive(Clone, Copy)]
pub struct Generation {
    pub id: u32,
    pub parent: u32,
    pub payload: u64,
    pub checksum: u64,
}

impl Generation {
    fn compute_checksum(id: u32, parent: u32, payload: u64) -> u64 {
        // FNV-1a, 64-bit, over the generation's own fields. Not a signature — see the module note. It exists so that
        // an edited generation is REFUSED rather than silently read.
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut hash = OFFSET;
        for byte in id.to_le_bytes().iter().chain(parent.to_le_bytes().iter()).chain(payload.to_le_bytes().iter()) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        hash
    }
}

/// The ledger. A fixed ring of generations — size is an M2 decision, and 16 is enough for a show's cue list with room
/// to spare before this becomes a real store.
const CAPACITY: usize = 16;

pub struct Ledger {
    gens: [Generation; CAPACITY],
    head: u32,
    next_id: u32,
}

#[derive(Debug)]
pub struct Tampered {
    pub generation: u32,
}

#[derive(Debug)]
pub struct NoSuchGeneration;

impl Ledger {
    pub const fn empty() -> Self {
        Self {
            gens: [Generation { id: 0, parent: 0, payload: 0, checksum: 0 }; CAPACITY],
            head: 0,
            next_id: 0,
        }
    }

    /// Append a generation. This is the only way state changes: the parent is whatever the head was, so the chain
    /// cannot be spliced by a caller.
    pub fn apply(&mut self, payload: u64) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        let parent = if id == 0 { 0 } else { self.head };
        let slot = (id as usize) % CAPACITY;
        self.gens[slot] = Generation {
            id,
            parent,
            payload,
            checksum: Generation::compute_checksum(id, parent, payload),
        };
        self.head = id;
        id
    }

    /// Append a generation ONLY if it carries a certificate the Core accepts. This is the rung's whole point: a
    /// generation that cannot prove it fits the deadline budget does not become history at all — the ledger's head does
    /// not move, and the refusal names the term that broke it.
    pub fn apply_certified(
        &mut self,
        payload: u64,
        cert: &crate::cert::Certificate,
    ) -> Result<(u32, u64), crate::cert::Rejected> {
        let ppm = crate::cert::check(cert)?;
        Ok((self.apply(payload), ppm))
    }

    /// The current generation's id, so a refusal can show that the head did not move.
    pub fn head_id(&self) -> u32 {
        self.head
    }

    /// Read the current state — but only if its generation checks out. A corrupted head is refused, not returned.
    pub fn read(&self, generation: u32) -> Result<u64, Tampered> {
        let g = self.slot(generation).ok_or(Tampered { generation })?;
        if g.checksum != Generation::compute_checksum(g.id, g.parent, g.payload) {
            return Err(Tampered { generation });
        }
        Ok(g.payload)
    }

    /// Undo `steps` generations without a reboot: walk down the parent chain. The old generations stay — an undo is a
    /// move of the head, not a deletion, which is what makes it reversible itself.
    pub fn undo(&mut self, steps: u32) -> Result<u32, NoSuchGeneration> {
        let mut cursor = self.head;
        for _ in 0..steps {
            let g = self.slot(cursor).ok_or(NoSuchGeneration)?;
            if g.parent == cursor && cursor != 0 {
                return Err(NoSuchGeneration);
            }
            cursor = g.parent;
        }
        self.head = cursor;
        Ok(cursor)
    }

    fn slot(&self, generation: u32) -> Option<&Generation> {
        if generation >= self.next_id {
            return None;
        }
        let g = &self.gens[(generation as usize) % CAPACITY];
        if g.id != generation {
            return None;
        }
        Some(g)
    }

    /// Corrupt a generation's payload in place, without touching its checksum. This is the attack the falsifier runs:
    /// it is exactly what an attacker with write access to the store would do, and exactly what must be refused.
    #[cfg(feature = "m2-test-tamper")]
    pub fn corrupt_for_test(&mut self, generation: u32) {
        if self.slot(generation).is_some() {
            let slot = (generation as usize) % CAPACITY;
            self.gens[slot].payload ^= 0xdead_beef;
        }
    }
}

/// Whether verification is in force. `m2-mutant-skip-verify` disables it, which is the mutation the falsifier's
/// self-test turns on: with verification off, the tampered generation must be ACCEPTED and the check must go red.
fn verifying() -> bool {
    !cfg!(feature = "m2-mutant-skip-verify")
}

/// The Core's ledger decisions, written to the serial line so a test — and a person — can read what the machine did
/// and what it refused.
pub fn report(uart: &Uart, ledger: &mut Ledger) {
    uart.write_line("M2: the ledger — state as generations");

    let a = ledger.apply(0xAAAA);
    let b = ledger.apply(0xBBBB);
    uart.write_str("M2: applied generation ");
    write_u32(uart, b);
    uart.write_str(" (parent ");
    write_u32(uart, a);
    uart.write_line(")");

    let now = ledger.read(b).expect("the head just written must verify");
    if now == 0xBBBB {
        uart.write_line("M2: read head -> 0xBBBB ok");
    }

    let back = ledger.undo(1).expect("undo of one step must land on the parent");
    match ledger.read(back) {
        Ok(v) if v == 0xAAAA => uart.write_line("M2: undo 1 -> generation 0 reads 0xAAAA ok (no reboot)"),
        _ => uart.write_line("M2: FAIL undo did not land on the parent"),
    }

    // Re-apply so the chain has a head again, then run the attack the falsifier requires.
    let c = ledger.apply(0xCCCC);
    uart.write_str("M2: applied generation ");
    write_u32(uart, c);
    uart.write_line("");

    #[cfg(feature = "m2-test-tamper")]
    {
        ledger.corrupt_for_test(0);
        uart.write_line("M2: ATTACK — generation 0 corrupted in place, checksum left alone");
        let verdict = if verifying() {
            match ledger.read(0) {
                Ok(_) => {
                    uart.write_line("M2: FAIL tampered generation 0 was ACCEPTED");
                    false
                }
                Err(t) => {
                    uart.write_str("M2: TAMPERED generation ");
                    write_u32(uart, t.generation);
                    uart.write_line(" REFUSED (checksum does not match)");
                    true
                }
            }
        } else {
            uart.write_line("M2: (verification is DISABLED by the mutation feature)");
            false
        };
        if verdict {
            uart.write_line("M2: the machine refused a generation that does not check out");
        }
    }
}

fn write_u32(uart: &Uart, value: u32) {
    // Small enough for M2 that a decimal formatter would be more code than the thing it prints.
    let mut buf = [0u8; 10];
    let mut len = 0;
    let mut v = value;
    if v == 0 {
        uart.write_byte(b'0');
        return;
    }
    while v > 0 && len < buf.len() {
        buf[len] = b'0' + (v % 10) as u8;
        v /= 10;
        len += 1;
    }
    while len > 0 {
        len -= 1;
        uart.write_byte(buf[len]);
    }
}
