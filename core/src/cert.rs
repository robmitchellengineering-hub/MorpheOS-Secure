//! The schedulability certificate — rung M7a, from pacland's method: exact arithmetic, a checkable witness, no
//! measurement.
//!
//! THE CLAIM: whether a set of periodic tasks fits its deadlines is an EXACT RATIONAL INEQUALITY, and the Core can
//! check it in bounded time with no allocation and no floating point:
//!
//!     Σ (Cᵢ / Tᵢ) ≤ 1        (EDF utilisation test; deadlines = periods)
//!
//! Checked by cross-multiplication: with D = Π Tⱼ and S = Σ Cᵢ·(D / Tᵢ), the claim is `S ≤ D` — integers throughout.
//!
//! WHY BOUNDED, AND WHAT THE BOUND IS. A certificate is a witness, and a witness a hostile party can make expensive is
//! a denial-of-service primitive. So: at most three terms, each cost and period below 2³², which keeps `D ≤ 2⁹⁶` and
//! `S ≤ 3·2⁹⁶` inside `u128`. A certificate outside those bounds is REFUSED, not computed.
//!
//! WHAT A CERTIFICATE IS NOT: a proof that the graph is right. It is a proof that the arithmetic holds — the Core
//! checks, and the criterion itself is a theorem (the EDF utilisation bound) rather than a movement's opinion.

/// One periodic task's terms, as the certificate states them. `name` is for the refusal: a rejection that cannot say
/// which task broke the budget is not a rejection a person can act on.
#[derive(Clone, Copy)]
pub struct Term {
    pub name: &'static str,
    pub cost: u64,
    pub period: u64,
}

/// The certificate handed to the Core.
#[derive(Clone, Copy)]
pub struct Certificate {
    pub terms: [Term; MAX_TERMS],
    pub len: usize,
    /// What the producer claims the utilisation is, in parts per million. The Core recomputes it; a disagreement is
    /// LYING, and is refused like any other failed check.
    pub claimed_ppm: u64,
}

pub const MAX_TERMS: usize = 3;
/// The largest cost or period a certificate may state. Beyond this the check is not bounded.
pub const MAX_TERM: u64 = 1 << 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rejected {
    /// More terms than the bounded check accepts.
    TooManyTerms,
    /// A cost or period outside the bound; refusing is the only bounded answer.
    TermTooLarge,
    /// Zero cost or period is not a schedule, it is a malformed certificate.
    Malformed,
    /// The utilisation exceeds 1. Carries the term that contributed most, which is the one to fix.
    OverBudget { largest: &'static str },
    /// The stated utilisation disagrees with the arithmetic.
    Lying { claimed_ppm: u64, actual_ppm: u64 },
}

/// Check a certificate. Returns the utilisation in parts per million when it fits.
///
/// No allocation, no division by anything but the fixed denominators of the arithmetic, and every multiplication is
/// bounds-checked before it happens.
pub fn check(cert: &Certificate) -> Result<u64, Rejected> {
    if cert.len == 0 || cert.len > MAX_TERMS {
        return Err(if cert.len > MAX_TERMS { Rejected::TooManyTerms } else { Rejected::Malformed });
    }
    let n = cert.len;
    let mut denominator: u128 = 1;
    for i in 0..n {
        let t = cert.terms[i];
        if t.cost == 0 || t.period == 0 {
            return Err(Rejected::Malformed);
        }
        if t.cost >= MAX_TERM || t.period >= MAX_TERM {
            return Err(Rejected::TermTooLarge);
        }
        // The bound above (three terms, each < 2³²) keeps this product under 2⁹⁶.
        denominator *= t.period as u128;
    }
    let mut numerator: u128 = 0;
    let mut largest_share: u128 = 0;
    let mut largest: &'static str = "";
    for i in 0..n {
        let t = cert.terms[i];
        let share = (t.cost as u128) * (denominator / t.period as u128);
        numerator += share;
        if share > largest_share {
            largest_share = share;
            largest = t.name;
        }
    }
    // Per-million utilisation, computed exactly then rounded down — the certificate is compared against the same
    // computation, so a rounded figure can never be "lying" merely by rounding.
    let actual_ppm = ((numerator * 1_000_000) / denominator) as u64;
    if !cfg!(feature = "cert-mutant-skip-check") {
        if numerator > denominator {
            return Err(Rejected::OverBudget { largest });
        }
        if actual_ppm != cert.claimed_ppm {
            return Err(Rejected::Lying { claimed_ppm: cert.claimed_ppm, actual_ppm });
        }
    }
    Ok(actual_ppm)
}
