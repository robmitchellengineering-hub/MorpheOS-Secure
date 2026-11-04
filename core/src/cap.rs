//! The capability space — rung M1.
//!
//! THE CLAIM THIS FILE EXISTS TO MAKE: authority in this machine is **an object a task was handed**, not a user who
//! is allowed to do things. There is no "root" to escalate to, because there is no ambient authority to escalate
//! from: a task naming an object it does not hold is refused by the Core, and the refusal names both the task and the
//! object.
//!
//! WHAT THIS IS NOT YET: the tasks here are cooperative functions inside the Core, not isolated address spaces (that
//! needs the MMU, which is M0b on the Pi). So the refusal is real but the *containment* is not — a hostile task in the
//! same address space could write the table. Isolation arrives with the MMU, and this file says so rather than
//! implying otherwise.
//!
//! The falsifier is the point: `scripts/m1.sh` requires the refusal line, and its `--self-test` builds the same code
//! with the capability **granted** (feature `m1-mutant-grant`) and requires the refusal to be ABSENT. A check that
//! cannot be made to pass is not checking the Core's table — it is checking a hard-coded string.

use crate::uart::Uart;

/// An object the Core mediates access to. Ids are small and dense because the table is a bitmask on this rung; when
/// the object space grows past 64 this becomes a real table and the bitmask becomes a lookup, deliberately.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ObjectId(pub u8);

/// A capability: proof that a task was handed an object. It is deliberately a token with no methods that reach the
/// object — holding it is the whole authority, and losing it is the whole revocation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cap {
    pub object: ObjectId,
}

/// What the Core says when a task names an object it does not hold.
#[derive(Debug)]
pub struct Denied {
    pub task: &'static str,
    pub object: ObjectId,
}

/// A task's capability set. The only way to obtain a `Cap` is `grant`, which the Core performs — nothing in a task can
/// manufacture one, which is the property the whole design rests on.
#[derive(Clone, Copy, Default)]
pub struct CapTable {
    bits: u64,
}

impl CapTable {
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    /// The Core hands a task a capability. This is the only constructor of `Cap` in the machine.
    pub fn grant(&mut self, object: ObjectId) -> Cap {
        assert!(object.0 < 64, "object id outside the M1 table");
        self.bits |= 1 << object.0;
        Cap { object }
    }

    /// The check the Core performs on every access. It takes the *name* of the object, not a cap, because a task that
    /// holds a stale or forged cap must fail here exactly like a task that never had one.
    pub fn check(&self, task: &'static str, object: ObjectId) -> Result<Cap, Denied> {
        if object.0 < 64 && self.bits & (1 << object.0) != 0 {
            Ok(Cap { object })
        } else {
            Err(Denied { task, object })
        }
    }

    pub fn holds(&self, object: ObjectId) -> bool {
        object.0 < 64 && self.bits & (1 << object.0) != 0
    }
}

/// A named object with a value, so that "access" produces something a test can read rather than a comment.
pub struct Object {
    pub id: ObjectId,
    pub name: &'static str,
    pub value: u32,
}

/// What the Core does when a task asks for an object: allow it and return the object's value, or refuse it and write
/// the refusal to the serial line so the test — and a person — can read who asked for what.
pub fn access(
    uart: &Uart,
    task: &'static str,
    caps: &CapTable,
    object: &Object,
) -> Result<u32, Denied> {
    match caps.check(task, object.id) {
        Ok(cap) => {
            debug_assert_eq!(cap.object, object.id);
            uart.write_str("M1: ALLOWED  ");
            uart.write_str(task);
            uart.write_str(" -> object ");
            uart.write_str(object.name);
            uart.write_line("");
            Ok(object.value)
        }
        Err(denied) => {
            // The refusal is the Core's, and it names both parties. This line is what the test greps for and what a
            // person reads in a log: a refusal that cannot say who and what is not a refusal a person can act on.
            uart.write_str("M1: REFUSED  ");
            uart.write_str(denied.task);
            uart.write_str(" -> object ");
            uart.write_str(object.name);
            uart.write_str(" (no capability)");
            uart.write_line("");
            Err(denied)
        }
    }
}
