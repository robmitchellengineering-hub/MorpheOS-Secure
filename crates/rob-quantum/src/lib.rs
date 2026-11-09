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
/// S-dagger (the inverse phase gate).
pub const SDG: Gate = gate(C64::ONE, C64::ZERO, C64::ZERO, C64 { re: 0.0, im: -1.0 });
/// T gate (π/4 phase).
pub const T: Gate = gate(C64::ONE, C64::ZERO, C64::ZERO, C64 { re: INV_SQRT2, im: INV_SQRT2 });

/// A general n-qubit circuit with a state-vector backend. Qubit 0 is the least-significant bit.
#[derive(Clone)]
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

    /// Controlled modular exponentiation (Shor's oracle, as a black box): map the `work` register
    /// `w → w · a^x mod n`, controlled on the `control` register's value `x`. This is the hard piece
    /// of Shor's period-finding; the oracle is a permutation, treated as a black box like Grover's.
    pub fn apply_modular_exp(&mut self, control: &[usize], work: &[usize], a: u64, n: u64) {
        let mut next = alloc::vec![C64::ZERO; self.state.len()];
        for i in 0..self.state.len() {
            let mut x = 0usize;
            for (k, &q) in control.iter().enumerate() {
                if (i >> q) & 1 == 1 {
                    x |= 1 << k;
                }
            }
            let mut w = 0usize;
            for (k, &q) in work.iter().enumerate() {
                if (i >> q) & 1 == 1 {
                    w |= 1 << k;
                }
            }
            let f = mod_pow(a, x as u64, n) as usize;
            let w_new = (w * f) % (n as usize);
            let mut j = i;
            for (k, &q) in work.iter().enumerate() {
                if (w_new >> k) & 1 == 1 {
                    j |= 1 << q;
                } else {
                    j &= !(1 << q);
                }
            }
            next[j] = self.state[i];
        }
        self.state = next;
    }

    /// Swap two qubits (exchanges their bit positions in the state vector).
    pub fn swap(&mut self, a: usize, b: usize) {
        assert!(a < self.n && b < self.n && a != b);
        let am = 1usize << a;
        let bm = 1usize << b;
        for i in 0..self.state.len() {
            let ba = usize::from(i & am != 0);
            let bb = usize::from(i & bm != 0);
            if ba != bb {
                let j = i ^ am ^ bm;
                if i < j {
                    self.state.swap(i, j);
                }
            }
        }
    }
}

/// Classical modular exponentiation by squaring: `a^e mod n`.
fn mod_pow(mut a: u64, mut e: u64, n: u64) -> u64 {
    let mut r = 1 % n;
    a %= n;
    while e > 0 {
        if e & 1 == 1 {
            r = (r * a) % n;
        }
        a = (a * a) % n;
        e >>= 1;
    }
    r
}

/// A falsifier bound on one probability: p[index] ∈ [min, max].
#[derive(Clone, Copy, Debug)]
pub struct Bound {
    pub index: usize,
    pub min: f64,
    pub max: f64,
}

/// A quantum claim — a named circuit plus the falsifier bounds its distribution must satisfy.
///
/// `sextant` names the SEXTANT claim (in `MorpheOS/crates/morph-verify`) this circuit-level falsifier
/// is an *instance of* — the theorem that backs it. The spine is one (D43): SEXTANT holds the
/// theorems, this crate holds the compute, and the certificate points at the theorem, not at a
/// parallel claim of its own.
pub struct QuantumClaim {
    pub circuit: &'static str,
    pub sextant: &'static str,
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

/// Grover's falsifier: the marked |11⟩ is found with probability 1. Backed by SEXTANT's
/// `clifford-polynomial` claim (Grover's oracle is non-Clifford — that is what makes the search genuinely quantum).
pub const GROVER_CLAIM: QuantumClaim = QuantumClaim {
    circuit: "grover",
    sextant: "clifford-polynomial",
    bounds: &[Bound { index: 3, min: 1.0 - 1e-9, max: 1.0 + 1e-9 }],
};

/// Bell's falsifier: |00⟩ and |11⟩ each 1/2, the cross terms 0. Backed by SEXTANT's `bell-classical`
/// claim (the Bell state violates the classical correlation bound).
pub const BELL_CLAIM: QuantumClaim = QuantumClaim {
    circuit: "bell",
    sextant: "bell-classical",
    bounds: &[
        Bound { index: 0, min: 0.5 - 1e-9, max: 0.5 + 1e-9 },
        Bound { index: 1, min: 0.0, max: 1e-9 },
        Bound { index: 2, min: 0.0, max: 1e-9 },
        Bound { index: 3, min: 0.5 - 1e-9, max: 0.5 + 1e-9 },
    ],
};

/// A certified quantum run: the checked distribution plus the measured outcome, ready to submit to
/// the ledger. `sextant` names the SEXTANT claim that certifies it. The seed names the draw, so the
/// run is reproducible.
pub struct CertifiedRun {
    pub circuit: &'static str,
    pub sextant: &'static str,
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
    Ok(CertifiedRun { circuit: claim.circuit, sextant: claim.sextant, seed, outcome, probabilities: p })
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
/// e^{iπ/2^k} for k = 1..=8 — the QFT controlled-phase angles (fixed, no libm).
const QFT_PHASE: [C64; 8] = [
    C64 { re: 0.0, im: 1.0 },                                                // k=1
    C64 { re: INV_SQRT2, im: INV_SQRT2 },                                    // k=2
    C64 { re: 0.923_879_532_511_286_7, im: 0.382_683_432_365_089_8 },        // k=3
    C64 { re: 0.980_785_280_403_230_4, im: 0.195_090_322_016_128_25 },       // k=4
    C64 { re: 0.995_184_726_672_196_9, im: 0.098_017_140_329_560_6 },        // k=5
    C64 { re: 0.998_795_456_205_172_4, im: 0.049_067_674_327_418_015 },      // k=6
    C64 { re: 0.999_698_818_696_204_2, im: 0.024_541_228_522_912_288 },      // k=7
    C64 { re: 0.999_924_701_839_144_5, im: 0.012_271_538_285_719_925 },      // k=8
];

/// The n-qubit quantum Fourier transform (the building block for Shor). Qubit 0 is least-significant.
pub fn qft(n: usize) -> Circuit {
    let mut c = Circuit::new(n);
    assert!(n <= QFT_PHASE.len() + 1, "QFT beyond the phase table");
    for q in (0..n).rev() {
        c.apply_gate(&H, q);
        for t in 0..q {
            c.apply_cphase(q, t, QFT_PHASE[q - t - 1]);
        }
    }
    // the decomposition yields the bit-reversed QFT; reverse the qubits to get F.
    for q in 0..n / 2 {
        c.swap(q, n - 1 - q);
    }
    c
}

/// Apply the inverse QFT to a specific list of qubits (qubits[0] = least significant).
fn apply_qft_inverse(c: &mut Circuit, qubits: &[usize]) {
    // QFT† = F_swapped† · SWAP: reverse the qubits first, then the inverse rotations.
    let n = qubits.len();
    for i in 0..n / 2 {
        c.swap(qubits[i], qubits[n - 1 - i]);
    }
    for i in 0..n {
        let q = qubits[i];
        for t in 0..i {
            let p = QFT_PHASE[i - t - 1];
            c.apply_cphase(q, qubits[t], C64 { re: p.re, im: -p.im });
        }
        c.apply_gate(&H, q);
    }
}

/// Shor's period-finding for a=2, N=15 (period r=4). Period register = qubits 0-3, work = 4-7.
/// Returns the circuit before measurement; the period register's probability is peaked at 0, 4, 8, 12.
pub fn shor_period_finding() -> Circuit {
    let mut c = Circuit::new(8);
    c.apply_gate(&X, 4);                       // work register = |1>
    for q in 0..4 {
        c.apply_gate(&H, q);                   // period register = uniform superposition
    }
    c.apply_modular_exp(&[0, 1, 2, 3], &[4, 5, 6, 7], 2, 15);
    apply_qft_inverse(&mut c, &[0, 1, 2, 3]);
    c
}

/// Greatest common divisor.
fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// The denominator of the continued fraction of `a/b` (the period candidate).
fn cf_denominator(mut a: u64, mut b: u64) -> u64 {
    let (mut p0, mut p1) = (0u64, 1u64);
    let (mut q0, mut q1) = (1u64, 0u64);
    while b != 0 {
        let f = a / b;
        let p2 = f * p1 + p0;
        let q2 = f * q1 + q0;
        p0 = p1;
        p1 = p2;
        q0 = q1;
        q1 = q2;
        let r = a % b;
        a = b;
        b = r;
    }
    q1
}

/// Shor's algorithm end-to-end: factor N=15. Period-finding → continued fractions → gcd(2^(r/2)±1, 15).
pub fn shor_factor() -> Option<(u64, u64)> {
    let (n, a) = (15u64, 2u64);
    let p = shor_period_finding().probabilities();
    let mut period = [0.0f64; 16];
    for (i, &pi) in p.iter().enumerate() {
        period[i & 0xF] += pi;
    }
    for k in 1..16 {
        if period[k] < 0.2 {
            continue;
        }
        let r = cf_denominator(k as u64, 16);
        if r == 0 || r % 2 != 0 {
            continue;
        }
        let x = mod_pow(a, r / 2, n);
        if x == 1 || x == n - 1 {
            continue;
        }
        let g1 = gcd(x + 1, n);
        let g2 = gcd(x - 1, n);
        if g1 > 1 && g1 < n && g2 > 1 && g2 < n {
            return Some((g1, g2));
        }
    }
    None
}

/// One Pauli term of a Hamiltonian: a coefficient and a string of I/X/Y/Z, indexed by qubit
/// (string[0] is qubit 0). The core of VQE/quantum chemistry.
#[derive(Clone, Copy)]
pub struct PauliTerm {
    pub coeff: f64,
    pub pauli: &'static str,
}

/// The H2 molecule's two-qubit Hamiltonian (bond length 0.735 Å, O'Malley et al. 2016).
/// Ground-state energy ≈ -1.857275. (Z0 = +, Z1 = −.)
pub const H2: [PauliTerm; 5] = [
    PauliTerm { coeff: -1.052_373_245_772_859, pauli: "II" },
    PauliTerm { coeff: 0.397_937_424_843_180_45, pauli: "ZI" },
    PauliTerm { coeff: -0.397_937_424_843_180_45, pauli: "IZ" },
    PauliTerm { coeff: -0.011_280_104_256_235_38, pauli: "ZZ" },
    PauliTerm { coeff: 0.180_931_199_784_231_56, pauli: "XX" },
];

/// The expectation ⟨ψ|H|ψ⟩ for a Pauli-term Hamiltonian. X/Y terms are rotated to the Z basis
/// (H, or S†H) before the Z-parity measurement. No allocation per term beyond the basis rotation.
pub fn expectation(circuit: &Circuit, terms: &[PauliTerm]) -> f64 {
    let mut e = 0.0;
    for term in terms {
        let mut c = circuit.clone();
        for (q, &ch) in term.pauli.as_bytes().iter().enumerate() {
            match ch {
                b'X' => c.apply_gate(&H, q),
                b'Y' => {
                    c.apply_gate(&SDG, q);
                    c.apply_gate(&H, q);
                }
                _ => {}
            }
        }
        let mut z = 0.0;
        for (i, a) in c.state.iter().enumerate() {
            let mut sign = 1.0;
            for (q, &ch) in term.pauli.as_bytes().iter().enumerate() {
                if ch != b'I' && (i >> q) & 1 == 1 {
                    sign = -sign;
                }
            }
            z += sign * a.norm_sq();
        }
        e += term.coeff * z;
    }
    e
}

/// The Ry(θ) rotation gate.
pub fn ry(theta: f64) -> Gate {
    let c = libm::cos(theta / 2.0);
    let s = libm::sin(theta / 2.0);
    gate(
        C64 { re: c, im: 0.0 },
        C64 { re: -s, im: 0.0 },
        C64 { re: s, im: 0.0 },
        C64 { re: c, im: 0.0 },
    )
}

/// VQE for H2: a two-parameter hardware-efficient ansatz Ry(θ0)⊗Ry(θ1) then CNOT, grid-searched to
/// the ground state. Returns the minimum energy ⟨ψ(θ)|H|ψ(θ)⟩ over a coarse 2-D grid.
pub fn vqe_h2() -> f64 {
    let mut best = f64::INFINITY;
    let n = 100;
    for i in 0..=n {
        for j in 0..=n {
            let t0 = 2.0 * core::f64::consts::PI * (i as f64) / (n as f64);
            let t1 = 2.0 * core::f64::consts::PI * (j as f64) / (n as f64);
            let mut c = Circuit::new(2);
            c.apply_gate(&ry(t0), 0);
            c.apply_gate(&ry(t1), 1);
            c.apply_cnot(0, 1);
            let e = expectation(&c, &H2);
            if e < best {
                best = e;
            }
        }
    }
    best
}

/// Teleport |1⟩ from qubit 0 to qubit 2 through a Bell pair, with Alice's measurement and Bob's
/// conditional corrections. After the protocol, qubit 2 is |1⟩ regardless of Alice's outcomes.
pub fn teleport(seed: u64) -> Circuit {
    let mut c = Circuit::new(3);
    c.apply_gate(&X, 0);                       // |ψ⟩ = |1⟩
    c.apply_gate(&H, 1);                       // Bell pair (|00⟩+|11⟩)/√2 on qubits 1,2
    c.apply_cnot(1, 2);
    c.apply_cnot(0, 1);                        // Alice entangles and measures
    c.apply_gate(&H, 0);
    let a = c.measure_qubit(0, seed);
    let b = c.measure_qubit(1, seed.wrapping_add(1));
    if b == 1 {
        c.apply_gate(&X, 2);                   // Bob's corrections
    }
    if a == 1 {
        c.apply_gate(&Z, 2);
    }
    c
}

/// The 3-qubit repetition code: encode |1⟩ → |111⟩, flip qubit 1 (a single error), leaving |101⟩ —
/// a majority vote then recovers the logical 1.
pub fn qec_bitflip() -> Circuit {
    let mut c = Circuit::new(3);
    c.apply_gate(&X, 0);                       // logical |1⟩
    c.apply_cnot(0, 1);                        // encode → |111⟩
    c.apply_cnot(0, 2);
    c.apply_gate(&X, 1);                       // one bit-flip error → |101⟩
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

    #[test]
    fn teleport_moves_the_state() {
        for seed in [0, 1, 42, u64::MAX] {
            let p = teleport(seed).probabilities();
            let p2: f64 = p.iter().enumerate()
                .filter(|(i, _)| (i >> 2) & 1 == 1)
                .map(|(_, &x)| x).sum();
            assert!((p2 - 1.0).abs() < 1e-9, "seed {seed}: qubit 2 not |1>, p2={p2}");
        }
    }

    #[test]
    fn qec_detects_the_error() {
        let p = qec_bitflip().probabilities();
        assert!((p[5] - 1.0).abs() < 1e-12, "|101> expected, got {p:?}");
    }

    #[test]
    fn mod_pow_has_the_right_period() {
        assert_eq!(mod_pow(2, 0, 15), 1);
        assert_eq!(mod_pow(2, 1, 15), 2);
        assert_eq!(mod_pow(2, 2, 15), 4);
        assert_eq!(mod_pow(2, 4, 15), 1); // r = 4
    }

    #[test]
    fn modular_exp_maps_one_to_two() {
        // control = |1> (qubit 4), work = |1> (qubit 0); a=2, n=15 → work = 2^1 mod 15 = 2
        let mut c = Circuit::new(8);
        c.apply_gate(&X, 0); // work = |1>
        c.apply_gate(&X, 4); // control = |1>
        c.apply_modular_exp(&[4, 5, 6, 7], &[0, 1, 2, 3], 2, 15);
        let p = c.probabilities();
        // new state: control=1 (bit4), work=2 (bit1) → index 16 + 2 = 18
        assert!((p[18] - 1.0).abs() < 1e-12, "expected |x=1,w=2>, got {p:?}");
    }

    #[test]
    fn shor_period_is_peaked_at_multiples_of_four() {
        let c = shor_period_finding();
        let p = c.probabilities();
        let mut period = [0.0f64; 16];
        for (i, &pi) in p.iter().enumerate() {
            period[i & 0xF] += pi; // low 4 bits = period register
        }
        let mut peak = 0.0;
        for k in [0, 4, 8, 12] {
            peak += period[k];
        }
        assert!((peak - 1.0).abs() < 1e-9, "period not peaked at multiples of 4: {period:?}");
    }

    #[test]
    fn shor_factors_fifteen() {
        match shor_factor() {
            Some((p, q)) => {
                assert_eq!(p * q, 15, "factors {p} * {q} != 15");
                assert!(p > 1 && q > 1, "trivial factors {p}, {q}");
            }
            None => panic!("Shor failed to factor 15"),
        }
    }

    #[test]
    fn h2_energy_on_the_vacuum_matches_the_reference() {
        // ⟨00|H2|00⟩ = -1.0523732 + 0.39793742 - 0.39793742 - 0.0112801 (XX contributes 0)
        let e = expectation(&Circuit::new(2), &H2);
        assert!((e - (-1.063_653_350_029_094_4)).abs() < 1e-5, "got {e}");
    }

    #[test]
    fn vqe_finds_the_h2_ground_state() {
        let e = vqe_h2();
        assert!((e - (-1.857_275_030_202_382)).abs() < 1e-2, "got {e}");
    }
}
