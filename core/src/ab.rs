//! The boot path's A/B — rung M0c: two generations, a ONE-SHOT trial, a commit on health, and a record that survives
//! the failure it describes.
//!
//! THE CLAIM: an update is tried, never assumed. A new generation boots once as a trial; if it does not report itself
//! healthy, the machine goes back to the generation that did — and the record says what was tried, so the failure is
//! not forgotten across the reboot it caused.
//!
//! WHY THE POLICY IS SEPARATE FROM THE MEDIA: this is a state machine, and a state machine can be proven correct
//! exhaustively without a storage driver. The driver's only job is to keep `Record` on the medium; getting the
//! decision right is the part that must never be wrong, because a boot loop is a machine nobody can use.
//!
//! WHAT THIS DOES NOT DO: it does not verify a signature, it does not roll back a partially written image, and it does
//! not touch hardware. Those are the driver and the image builder.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    A,
    B,
}

impl Slot {
    pub fn name(self) -> &'static str {
        match self {
            Slot::A => "A",
            Slot::B => "B",
        }
    }
    pub fn other(self) -> Slot {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }
    /// The id the record stores. `u32` because the record is a fixed-width structure on a medium, and a width
    /// disagreement between the type and its own record is a compile error — which is how this was caught.
    pub fn id(self) -> u32 {
        match self {
            Slot::A => 1,
            Slot::B => 2,
        }
    }
    pub fn from_id(id: u32) -> Option<Slot> {
        match id {
            1 => Some(Slot::A),
            2 => Some(Slot::B),
            _ => None,
        }
    }
}

/// What the medium carries. Every field is validated on read: a corrupt record falls back to the SAFE generation
/// rather than to a guess, because a machine that cannot read its own record must still boot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Record {
    pub magic: u32,
    pub active: u32,      // slot id that is known good
    pub trial: u32,       // slot id being tried, 0 for none
    pub attempts_left: u32,
    pub tried: u32,       // the slot that was tried and failed, 0 for none — the memory of the failure
}

pub const RECORD_MAGIC: u32 = 0x4D4F_5246; // "MORF"

impl Record {
    pub const fn fresh() -> Record {
        Record { magic: RECORD_MAGIC, active: 1, trial: 0, attempts_left: 0, tried: 0 }
    }

    /// Validate what came off the medium. Anything unrecognised yields a fresh record: `None` means "do not trust it".
    pub fn read(raw: Record) -> Record {
        if raw.magic != RECORD_MAGIC || Slot::from_id(raw.active).is_none() {
            return Record::fresh();
        }
        if raw.trial != 0 && Slot::from_id(raw.trial).is_none() {
            return Record::fresh();
        }
        raw
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Decision {
    pub boot: Slot,
    pub is_trial: bool,
    /// Set when the previous trial failed: the machine booted the other slot and remembers why.
    pub fell_back: bool,
}

/// Deciding what to boot. The one-shot rule lives here: a trial gets its attempt, and no second one without a commit.
pub fn choose(record: &Record) -> Decision {
    if record.trial != 0 {
        if let Some(trial) = Slot::from_id(record.trial) {
            if record.attempts_left > 0 {
                return Decision { boot: trial, is_trial: true, fell_back: false };
            }
            // The trial used its attempt and never committed: fall back, and keep the memory of it.
            let active = Slot::from_id(record.active).unwrap_or(Slot::A);
            return Decision { boot: active, is_trial: false, fell_back: true };
        }
    }
    Decision { boot: Slot::from_id(record.active).unwrap_or(Slot::A), is_trial: false, fell_back: false }
}

/// What the boot did about it. `healthy` is the machine's own report — the same line a person reads on the console.
pub fn settle(record: &mut Record, decision: Decision, healthy: bool) {
    if cfg!(feature = "ab-mutant-commit-always") {
        record.active = decision.boot.id();
        record.trial = 0;
        record.attempts_left = 0;
        return;
    }
    if decision.is_trial {
        if healthy {
            record.active = decision.boot.id();
            record.trial = 0;
            record.attempts_left = 0;
            record.tried = 0;
        } else {
            // One attempt, spent. Next boot falls back; the record says which slot failed.
            record.attempts_left = 0;
            record.tried = decision.boot.id();
        }
    }
}

/// Offer a new generation as a one-shot trial. Refuses to trial the slot that is already active.
pub fn propose(record: &mut Record, candidate: Slot) -> bool {
    if candidate.id() == record.active {
        return false;
    }
    record.trial = candidate.id();
    record.attempts_left = 1;
    true
}
