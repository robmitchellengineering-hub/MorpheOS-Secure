//! Box admission — rung M7d: validate a box ONCE at load, so its accesses need no per-access mask.
//!
//! THE CLAIM, in the shape software fault isolation turns on: if a box's code is validated at load to touch only the
//! memory the box was given, then it can run WITHOUT a mask or a bound check on every access — and those checks are
//! the 5–30% a sandbox otherwise costs.
//!
//! WHAT THIS MODELS, HONESTLY: the *admission shape*, not an ELF verifier. A box states where its code lives, how much
//! of the address space it will touch, and names that claim in a certificate; the Core checks the claim against the
//! box's limit ONCE. Writing the verifier that proves the claim of real machine code is M7d's real work and is not
//! here; what is here is the interface the rest of the Core will use, and the falsifier that a lying claim is refused.
//!
//! WHY IT MATTERS FOR A SHOW: every plugin, node session and control surface will arrive as foreign code. Checking a
//! claim once at load is the difference between a box that costs a few percent and one that costs a few percent per
//! memory access, at 1 ms deadlines.

/// What a box claims about itself, handed to the Core at load.
#[derive(Clone, Copy)]
pub struct Claim {
    /// The highest address the code says it will touch, and the certificate's whole content.
    pub highest_address: u64,
    /// Where the claim came from, so a refusal can name it: a producer, a signature, a manifest.
    pub signer: &'static str,
}

#[derive(Clone, Copy)]
pub struct BoxRequest {
    pub name: &'static str,
    /// Where the box's code sits, and how long it is.
    pub code_start: u64,
    pub code_len: u64,
    /// The limit the Core granted this box.
    pub limit: u64,
    pub claim: Claim,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Admitted {
    pub name: &'static str,
    /// The claim that was checked, kept so the admission can be audited later without re-running the check.
    pub checked_against: u64,
    pub signer: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// The code itself sits outside the box.
    CodeOutside { end: u64, limit: u64 },
    /// The claim exceeds the limit, so the box would touch memory it was not granted.
    ClaimOutside { claimed: u64, limit: u64, signer: &'static str },
}

/// Validate a box's claim ONCE. On success the box may run without per-access checks, because the claim it made about
/// its own addresses has been checked against the limit the Core granted.
pub fn admit(request: &BoxRequest) -> Result<Admitted, Refused> {
    let code_end = request.code_start.saturating_add(request.code_len);
    if code_end > request.limit {
        return Err(Refused::CodeOutside { end: code_end, limit: request.limit });
    }
    // The mutation: trust the claim without checking it, which is the mistake the whole rung exists to forbid.
    if !cfg!(feature = "box-mutant-trust-claim") && request.claim.highest_address > request.limit {
        return Err(Refused::ClaimOutside {
            claimed: request.claim.highest_address,
            limit: request.limit,
            signer: request.claim.signer,
        });
    }
    Ok(Admitted {
        name: request.name,
        checked_against: request.limit,
        signer: request.claim.signer,
    })
}
