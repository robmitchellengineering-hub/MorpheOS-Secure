//! The AirMatrix routing and mix engine core (spec, "Matrix and mix engine";
//! design note `docs/design/08-matrix-engine.md`).
//!
//! Any source to any destination, with a gain on every crosspoint, buses that
//! sum, buses feeding buses, and **mix-minus**: a feed from a bus that leaves one
//! participant out, so a caller, an IFB or a remote contributor never hears
//! themselves back.
//!
//! * [`MatrixControl`] (UI or network thread): validates and compiles changes.
//!   Feedback loops and mix-minus leaks are refused with the path named.
//! * [`MatrixAudio`] (audio thread): mixes, allocation-free and lock-free.
//!
//! Every change is click-free: gains, new routes, removed routes and
//! exclusions all move on a linear ramp (`ramp_frames`, 10 ms by default).
//! Only routes that sound cost CPU.
//!
//! ```
//! use rob_matrix::{matrix, MatrixConfig, Node};
//! let (mut ctl, mut audio) = matrix(MatrixConfig::new(2, 2));
//! let conf = ctl.add_bus("Conference").unwrap();
//! ctl.connect(Node::Input(0), conf, 0.0).unwrap();
//! ctl.connect(Node::Input(1), conf, 0.0).unwrap();
//! // Each caller hears the conference without themselves.
//! ctl.mix_minus(conf, Node::Input(0), Node::Output(0), 0.0).unwrap();
//! ctl.mix_minus(conf, Node::Input(1), Node::Output(1), 0.0).unwrap();
//! let (a, b) = ([0.5f32; 64], [0.25f32; 64]);
//! let (mut o0, mut o1) = ([0.0f32; 64], [0.0f32; 64]);
//! for _ in 0..20 {
//!     audio.process(&[&a, &b], &mut [&mut o0, &mut o1], 64);
//! }
//! assert!((o0[63] - 0.25).abs() < 1e-6 && (o1[63] - 0.5).abs() < 1e-6);
//! ```

#![cfg_attr(not(test), no_std)]

#[macro_use]
extern crate alloc;

/// f64/f32 float methods that live in `std`, re-provided over `libm` so the
/// crate links without std. In a std build the inherent methods win; in no_std
/// these are the only ones. Files bring it in via `use crate::prelude::*;`.
pub trait FloatExt: Sized {
    fn sin(self) -> Self;
    fn cos(self) -> Self;
    fn tan(self) -> Self;
    fn round(self) -> Self;
    fn floor(self) -> Self;
    fn ceil(self) -> Self;
    fn trunc(self) -> Self;
    fn exp(self) -> Self;
    fn ln(self) -> Self;
    fn log10(self) -> Self;
    fn powf(self, e: Self) -> Self;
    fn powi(self, e: i32) -> Self;
    fn rem_euclid(self, rhs: Self) -> Self;
    fn div_euclid(self, rhs: Self) -> Self;
    fn atan2(self, rhs: Self) -> Self;
    fn sqrt(self) -> Self;
}

impl FloatExt for f64 {
    fn sin(self) -> f64 {
        libm::sin(self)
    }
    fn cos(self) -> f64 {
        libm::cos(self)
    }
    fn tan(self) -> f64 {
        libm::tan(self)
    }
    fn round(self) -> f64 {
        libm::round(self)
    }
    fn floor(self) -> f64 {
        libm::floor(self)
    }
    fn ceil(self) -> f64 {
        libm::ceil(self)
    }
    fn trunc(self) -> f64 {
        libm::trunc(self)
    }
    fn exp(self) -> f64 {
        libm::exp(self)
    }
    fn ln(self) -> f64 {
        libm::log(self)
    }
    fn log10(self) -> f64 {
        libm::log10(self)
    }
    fn powf(self, e: f64) -> f64 {
        libm::pow(self, e)
    }
    fn powi(self, e: i32) -> f64 {
        // Repeated squaring: exact for integer exponents (libm has no powi).
        let mut b = self;
        let mut n = e.unsigned_abs();
        let mut r = 1.0f64;
        while n > 0 {
            if n & 1 == 1 {
                r *= b;
            }
            b *= b;
            n >>= 1;
        }
        if e < 0 {
            1.0 / r
        } else {
            r
        }
    }
    fn rem_euclid(self, rhs: f64) -> f64 {
        let r = self % rhs;
        if r < 0.0 {
            r + rhs.abs()
        } else {
            r
        }
    }
    fn div_euclid(self, rhs: f64) -> f64 {
        let r = self % rhs;
        let r = if r < 0.0 { r + rhs.abs() } else { r };
        (self - r) / rhs
    }
    fn atan2(self, rhs: f64) -> f64 {
        libm::atan2(self, rhs)
    }
    fn sqrt(self) -> f64 {
        libm::sqrt(self)
    }
}

impl FloatExt for f32 {
    fn sin(self) -> f32 {
        libm::sinf(self)
    }
    fn cos(self) -> f32 {
        libm::cosf(self)
    }
    fn tan(self) -> f32 {
        libm::tanf(self)
    }
    fn round(self) -> f32 {
        libm::roundf(self)
    }
    fn floor(self) -> f32 {
        libm::floorf(self)
    }
    fn ceil(self) -> f32 {
        libm::ceilf(self)
    }
    fn trunc(self) -> f32 {
        libm::truncf(self)
    }
    fn exp(self) -> f32 {
        libm::expf(self)
    }
    fn ln(self) -> f32 {
        libm::logf(self)
    }
    fn log10(self) -> f32 {
        libm::log10f(self)
    }
    fn powf(self, e: f32) -> f32 {
        libm::powf(self, e)
    }
    fn powi(self, e: i32) -> f32 {
        let mut b = self;
        let mut n = e.unsigned_abs();
        let mut r = 1.0f32;
        while n > 0 {
            if n & 1 == 1 {
                r *= b;
            }
            b *= b;
            n >>= 1;
        }
        if e < 0 {
            1.0 / r
        } else {
            r
        }
    }
    fn rem_euclid(self, rhs: f32) -> f32 {
        let r = self % rhs;
        if r < 0.0 {
            r + rhs.abs()
        } else {
            r
        }
    }
    fn div_euclid(self, rhs: f32) -> f32 {
        let r = self % rhs;
        let r = if r < 0.0 { r + rhs.abs() } else { r };
        (self - r) / rhs
    }
    fn atan2(self, rhs: f32) -> f32 {
        libm::atan2f(self, rhs)
    }
    fn sqrt(self) -> f32 {
        libm::sqrtf(self)
    }
}

/// The alloc items the crate's modules need unqualified. `use crate::prelude::*;`
/// is a glob import, so an unused member does not warn, and there is no name
/// collision with the `use alloc::collections::…` imports the modules keep.
pub mod prelude {
    pub use super::FloatExt;
    pub use alloc::boxed::Box;
    pub use alloc::string::{String, ToString};
    pub use alloc::vec::Vec;
}

mod audio;
mod control;
mod correct;
mod plan;

pub use audio::{AudioStats, MatrixAudio};
pub use control::{
    matrix, Change, CrosspointInfo, MatrixConfig, MatrixControl, MatrixError, Node, MAX_GAIN_DB,
};
pub use correct::Correction;
