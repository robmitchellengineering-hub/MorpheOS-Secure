#![cfg_attr(not(test), no_std)]
//! The quantum circuit model in no_std Rust — the Rust port of `pacland`'s `quantum.py`.
//!
//! A general n-qubit circuit: allocate a state vector, apply gates, measure. This is the compute
//! the Core hosts as a task (roadmap item 2 of `PLANS/PLAN-quantum-path.md`); the same model
//! targets real quantum hardware later. No floating-point in a *decision* here — the probabilities
//! are f64, but a verdict a caller derives from them is its own certificate, not this crate's.

extern crate alloc;
use alloc::vec::Vec;

const INV_SQRT2: f64 = 0.707_106_781_186_547_6; // 1/√2

/// A complex number (minimal, no_std, no dependency).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct C64 {
    pub re: f64,
    pub im: f64,
}

impl C64 {
    pub const ZERO: C64 = C64 { re: 0.0, im: 0.0 };
    pub const ONE: C64 = C64 { re: 1.0, im: 0.0 };
    pub const I: C64 = C64 { re: 0.0, im: 1.0 };

    pub fn add(self, o: C64) -> C64 {
        C64 { re: self.re + o.re, im: self.im + o.im }
    }
    pub fn mul(self, o: C64) -> C64 {
        C64 {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
    pub fn scale(self, s: f64) -> C64 {
        C64 { re: self.re * s, im: self.im * s }
    }
    pub fn norm_sq(self) -> f64 {
        self.re * self.re + self.im * self.im
    }
}

/// A single-qubit gate as a 2x2 complex matrix, row-major: [m00, m01, m10, m11].
#[derive(Clone, Copy, Debug)]
pub struct Gate {
    pub m: [C64; 4],
}

const fn gate(m00: C64, m01: C64, m10: C64, m11: C64) -> Gate {
    Gate { m: [m00, m01, m10, m11] }
}

/// Hadamard.
pub const H: Gate = gate(
    C64 { re: INV_SQRT2, im: 0.0 }, C64 { re: INV_SQRT2, im: 0.0 },
    C64 { re: INV_SQRT2, im: 0.0 }, C64 { re: -INV_SQRT2, im: 0.0 },
);
/// Pauli-X (bit flip).
pub const X: Gate = gate(C64::ZERO, C64::ONE, C64::ONE, C64::ZERO);
/// Pauli-Y.
pub const Y: Gate = gate(C64::ZERO, C64 { re: 0.0, im: -1.0 }, C64::I, C64::ZERO);
/// Pauli-Z (phase flip).
pub const Z: Gate = gate(C64::ONE, C64::ZERO, C64::ZERO, C64 { re: -1.0, im: 0.0 });
/// Phase gate (S).
pub const S: Gate = gate(C64::ONE, C64::ZERO, C64::ZERO, C64::I);
/// T gate (π/4 phase).
pub const T: Gate = gate(C64::ONE, C64::ZERO, C64::ZERO, C64 { re: INV_SQRT2, im: INV_SQRT2 });

/// A general n-qubit circuit with a state-vector backend. Qubit 0 is the least-significant bit.
pub struct Circuit {
    pub n: usize,
    pub state: Vec<C64>,
}

impl Circuit {
    pub fn new(n: usize) -> Circuit {
        assert!(n <= 24, "{n} qubits = {} amplitudes — RAM-bounded (D41)", 1usize << n);
        let mut state = alloc::vec![C64::ZERO; 1 << n];
        state[0] = C64::ONE; // |0…0>
        Circuit { n, state }
    }

    /// Apply a single-qubit gate to `target` (bit position; 0 = least significant).
    pub fn apply_gate(&mut self, g: &Gate, target: usize) {
        assert!(target < self.n);
        let mask = 1usize << target;
        let mut next = alloc::vec![C64::ZERO; self.state.len()];
        for i in 0..self.state.len() {
            if i & mask == 0 {
                let i1 = i | mask;
                next[i] = g.m[0].mul(self.state[i]).add(g.m[1].mul(self.state[i1]));
                next[i1] = g.m[2].mul(self.state[i]).add(g.m[3].mul(self.state[i1]));
            }
        }
        self.state = next;
    }

    /// CNOT: flip `target` where `control` is |1>.
    pub fn apply_cnot(&mut self, control: usize, target: usize) {
        assert!(control < self.n && target < self.n && control != target);
        let cmask = 1usize << control;
        let tmask = 1usize << target;
        let mut next = alloc::vec![C64::ZERO; self.state.len()];
        for i in 0..self.state.len() {
            let j = if i & cmask != 0 { i ^ tmask } else { i };
            next[j] = self.state[i];
        }
        self.state = next;
    }

    /// Controlled phase: multiply the |11⟩ amplitude on (control,target) by `phase`.
    pub fn apply_cphase(&mut self, control: usize, target: usize, phase: C64) {
        assert!(control < self.n && target < self.n && control != target);
        let cmask = 1usize << control;
        let tmask = 1usize << target;
        for i in 0..self.state.len() {
            if i & cmask != 0 && i & tmask != 0 {
                self.state[i] = self.state[i].mul(phase);
            }
        }
    }

    /// The |amplitude|^2 distribution, normalized (deterministic — no RNG in no_std).
    pub fn probabilities(&self) -> Vec<f64> {
        let mut p: Vec<f64> = self.state.iter().map(|a| a.norm_sq()).collect();
        let total: f64 = p.iter().sum();
        if total > 0.0 {
            for x in &mut p {
                *x /= total;
            }
        }
        p
    }

    /// Sample one bitstring from the |amplitude|² distribution, seeded and deterministic.
    ///
    /// The seed is part of the certificate: a verifier re-runs with the same seed and checks the
    /// same result, so "this run produced X" is reproducible rather than a measurement of one
    /// machine. (A nondeterministic sampler is a thin layer on top, fed entropy by the Core.)
    pub fn measure_all(&self, seed: u64) -> usize {
        let p = self.probabilities();
        let r = (seed >> 11) as f64 / (1u64 << 53) as f64; // uniform in [0, 1)
        let mut acc = 0.0;
        for (i, pi) in p.iter().enumerate() {
            acc += pi;
            if r < acc {
                return i;
            }
        }
        p.len() - 1
    }

    /// Measure one qubit: collapse the state onto the outcome (seeded, deterministic) and return it.
    /// This is the primitive teleportation and QEC need — a partial measurement, not a full one.
    ///
    /// The state is left scaled by the outcome's probability; `probabilities()` renormalizes, so the
    /// observable distribution is correct either way (no `sqrt` needed in no_std).
    pub fn measure_qubit(&mut self, target: usize, seed: u64) -> usize {
        assert!(target < self.n);
        let mask = 1usize << target;
        let mut p0 = 0.0;
        let mut p1 = 0.0;
        for i in 0..self.state.len() {
            let n = self.state[i].norm_sq();
            if i & mask != 0 {
                p1 += n;
            } else {
                p0 += n;
            }
        }
        let total = p0 + p1;
        let r = (seed >> 11) as f64 / (1u64 << 53) as f64; // uniform in [0, 1)
        let outcome = if r < p1 / total { 1 } else { 0 };
        for i in 0..self.state.len() {
            if usize::from(i & mask != 0) != outcome {
                self.state[i] = C64::ZERO;
            }
        }
        outcome
    }
}

/// A falsifier bound on one probability: p[index] ∈ [min, max].
#[derive(Clone, Copy, Debug)]
pub struct Bound {
    pub index: usize,
    pub min: f64,
    pub max: f64,
}

/// A quantum claim — a named circuit plus the falsifier bounds its distribution must satisfy.
/// This is the "SEXTANT falsifier → certificate" translation (PLAN-quantum-path.md §3): a run is
/// certified when its measured distribution satisfies every bound, and refused when one fails.
pub struct QuantumClaim {
    pub circuit: &'static str,
    pub bounds: &'static [Bound],
}

/// The ways a quantum claim is refused.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Rejected {
    NotNormalized,
    BoundViolated { index: usize, got: f64, min: f64, max: f64 },
}

impl QuantumClaim {
    /// Check the claim against a probability distribution. No allocation, no floating point in a
    /// *decision* beyond the tolerance the claim itself states.
    pub fn check(&self, p: &[f64]) -> Result<(), Rejected> {
        let total: f64 = p.iter().sum();
        if (total - 1.0).abs() > 1e-9 {
            return Err(Rejected::NotNormalized);
        }
        for b in self.bounds {
            let got = p[b.index];
            if got < b.min - 1e-12 || got > b.max + 1e-12 {
                return Err(Rejected::BoundViolated { index: b.index, got, min: b.min, max: b.max });
            }
        }
        Ok(())
    }
}

/// Grover's falsifier: the marked |11⟩ is found with probability 1.
pub const GROVER_CLAIM: QuantumClaim = QuantumClaim {
    circuit: "grover",
    bounds: &[Bound { index: 3, min: 1.0 - 1e-9, max: 1.0 + 1e-9 }],
};

/// Bell's falsifier: |00⟩ and |11⟩ each 1/2, the cross terms 0.
pub const BELL_CLAIM: QuantumClaim = QuantumClaim {
    circuit: "bell",
    bounds: &[
        Bound { index: 0, min: 0.5 - 1e-9, max: 0.5 + 1e-9 },
        Bound { index: 1, min: 0.0, max: 1e-9 },
        Bound { index: 2, min: 0.0, max: 1e-9 },
        Bound { index: 3, min: 0.5 - 1e-9, max: 0.5 + 1e-9 },
    ],
};

/// A certified quantum run: the checked distribution plus the measured outcome, ready to submit to
/// the ledger. The seed names the draw, so the run is reproducible.
pub struct CertifiedRun {
    pub circuit: &'static str,
    pub seed: u64,
    pub outcome: usize,
    pub probabilities: Vec<f64>,
}

/// Run the circuit, check the claim against its distribution, and if it holds produce a
/// `CertifiedRun` (the outcome sampled with `seed`). Refused with the claim's rejection otherwise.
/// This is the quantum half of the integration: the program proves, the Core records.
pub fn certify(claim: &QuantumClaim, circuit: &Circuit, seed: u64) -> Result<CertifiedRun, Rejected> {
    let p = circuit.probabilities();
    claim.check(&p)?;
    let outcome = circuit.measure_all(seed);
    Ok(CertifiedRun { circuit: claim.circuit, seed, outcome, probabilities: p })
}

/// The Bell state (|00⟩ + |11⟩)/√2.
pub fn bell() -> Circuit {
    let mut c = Circuit::new(2);
    c.apply_gate(&H, 0);
    c.apply_cnot(0, 1);
    c
}

/// The GHZ state (|0…0⟩ + |1…1⟩)/√2 — n-qubit entanglement, measured all-equal.
pub fn ghz(n: usize) -> Circuit {
    let mut c = Circuit::new(n);
    c.apply_gate(&H, 0);
    for q in 1..n {
        c.apply_cnot(0, q);
    }
    c
}

/// Grover's search on 2 qubits (4 items) for the marked state |11⟩.
pub fn grover() -> Circuit {
    let mut c = Circuit::new(2);
    c.apply_gate(&H, 0);
    c.apply_gate(&H, 1);
    // oracle: CZ (phase-flip |11⟩)
    c.apply_gate(&H, 1);
    c.apply_cnot(0, 1);
    c.apply_gate(&H, 1);
    // diffusion: H X⊗X CZ X⊗X H (reflect about the mean)
    c.apply_gate(&H, 0);
    c.apply_gate(&H, 1);
    c.apply_gate(&X, 0);
    c.apply_gate(&X, 1);
    c.apply_gate(&H, 1);
    c.apply_cnot(0, 1);
    c.apply_gate(&H, 1);
    c.apply_gate(&X, 0);
    c.apply_gate(&X, 1);
    c.apply_gate(&H, 0);
    c.apply_gate(&H, 1);
    c
}

/// The n-qubit quantum Fourier transform (the building block for Shor). Qubit 0 is least-significant.
/// Phase angles are fixed constants (e^{iπ/2^k}), so no `libm` is needed; n ≤ 8 (Shor-15's range).
pub fn qft(n: usize) -> Circuit {
    // e^{iπ/2^k} for k = 1..=8
    const PHASE: [C64; 8] = [
        C64 { re: 0.0, im: 1.0 },                                                // k=1
        C64 { re: INV_SQRT2, im: INV_SQRT2 },                                    // k=2
        C64 { re: 0.923_879_532_511_286_7, im: 0.382_683_432_365_089_8 },        // k=3
        C64 { re: 0.980_785_280_403_230_4, im: 0.195_090_322_016_128_25 },       // k=4
        C64 { re: 0.995_184_726_672_196_9, im: 0.098_017_140_329_560_6 },        // k=5
        C64 { re: 0.998_795_456_205_172_4, im: 0.049_067_674_327_418_015 },      // k=6
        C64 { re: 0.999_698_818_696_204_2, im: 0.024_541_228_522_912_288 },      // k=7
        C64 { re: 0.999_924_701_839_144_5, im: 0.012_271_538_285_719_925 },      // k=8
    ];
    let mut c = Circuit::new(n);
    assert!(n <= PHASE.len() + 1, "QFT beyond the phase table");
    for q in (0..n).rev() {
        c.apply_gate(&H, q);
        for t in 0..q {
            c.apply_cphase(q, t, PHASE[q - t - 1]);
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bell_is_perfectly_correlated() {
        let p = bell().probabilities();
        assert!((p[0] - 0.5).abs() < 1e-12, "|00> = {}", p[0]);
        assert!((p[3] - 0.5).abs() < 1e-12, "|11> = {}", p[3]);
        assert!(p[1].abs() < 1e-12 && p[2].abs() < 1e-12);
    }

    #[test]
    fn x_flips_the_bit() {
        let mut c = Circuit::new(1);
        c.apply_gate(&X, 0);
        let p = c.probabilities();
        assert!((p[1] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn hadamard_is_50_50() {
        let mut c = Circuit::new(1);
        c.apply_gate(&H, 0);
        let p = c.probabilities();
        assert!((p[0] - 0.5).abs() < 1e-12 && (p[1] - 0.5).abs() < 1e-12);
    }

    #[test]
    fn ghz_is_all_equal() {
        let p = ghz(3).probabilities();
        assert!((p[0] - 0.5).abs() < 1e-12, "|000> = {}", p[0]);
        assert!((p[7] - 0.5).abs() < 1e-12, "|111> = {}", p[7]);
        assert!(p[1..7].iter().all(|&x| x.abs() < 1e-12));
    }

    #[test]
    fn grover_finds_the_marked_item() {
        let p = grover().probabilities();
        assert!((p[3] - 1.0).abs() < 1e-12, "|11> = {}", p[3]);
    }

    #[test]
    fn measurement_is_consistent_with_a_deterministic_state() {
        let mut c = Circuit::new(1);
        c.apply_gate(&X, 0); // |1>
        for seed in [0, 1, 42, u64::MAX] {
            assert_eq!(c.measure_all(seed), 1, "seed {seed}");
        }
    }

    #[test]
    fn grover_and_bell_claims_hold() {
        assert!(GROVER_CLAIM.check(&grover().probabilities()).is_ok());
        assert!(BELL_CLAIM.check(&bell().probabilities()).is_ok());
    }

    #[test]
    fn a_violated_bound_is_refused() {
        // Bell's |00⟩ bound requires 0.5; a uniform 0.25/0.25/0.25/0.25 violates it.
        let p = [0.25, 0.25, 0.25, 0.25];
        assert!(matches!(BELL_CLAIM.check(&p), Err(Rejected::BoundViolated { index: 0, .. })));
    }

    #[test]
    fn a_non_normalized_distribution_is_refused() {
        let p = [1.0, 0.0, 0.0, 1.0]; // sums to 2
        assert!(matches!(BELL_CLAIM.check(&p), Err(Rejected::NotNormalized)));
    }

    #[test]
    fn certify_produces_a_run_for_a_holding_claim() {
        let run = certify(&GROVER_CLAIM, &grover(), 0).unwrap();
        assert_eq!(run.circuit, "grover");
        assert_eq!(run.outcome, 3); // |11>
    }

    #[test]
    fn certify_refuses_a_violated_claim() {
        let mut c = Circuit::new(2);
        c.apply_gate(&X, 0); // |01> — Bell's |00>=1/2 bound is violated
        assert!(certify(&BELL_CLAIM, &c, 0).is_err());
    }

    #[test]
    fn qft2_of_zero_is_uniform() {
        let p = qft(2).probabilities();
        for pi in &p {
            assert!((pi - 0.25).abs() < 1e-12, "uniform expected, got {pi}");
        }
    }

    #[test]
    fn measure_qubit_on_a_deterministic_state() {
        for seed in [0, 1, 42, u64::MAX] {
            let mut c = Circuit::new(2);
            c.apply_gate(&X, 0); // |1> on qubit 0
            assert_eq!(c.measure_qubit(0, seed), 1, "seed {seed}");
            let p = c.probabilities();
            let p_q0_1 = p[1] + p[3]; // bit0 = 1
            assert!((p_q0_1 - 1.0).abs() < 1e-12, "collapse wrong");
        }
    }
}
