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
}
