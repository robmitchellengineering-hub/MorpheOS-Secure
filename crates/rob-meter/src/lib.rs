//! # rob-meter
//!
//! Metering and measurement for AirMatrix (design note
//! `docs/design/18-metering-and-measurement.md`). One library, used in two
//! places: the **mix engine's** operational meters (what the operator watches
//! while mixing) and the **network analyser and setup wizard's** engineering
//! tools (what an engineer uses to set up and tune the system).
//!
//! * [`loudness`]: ITU-R BS.1770-4 / EBU R128 loudness and true peak;
//! * [`level`]: sample peak with hold, PPM (DIN, Nordic, BBC, EBU), VU;
//! * [`stereo`]: phase correlation, balance, goniometer points;
//! * [`fft`], [`spectrum`]: FFT analyser, 1/3-octave RTA, spectrogram;
//! * [`transfer`]: dual-channel transfer function and delay finder (system
//!   tuning).
//!
//! Meters that run on the audio thread allocate only in `new`.

// Signal-processing loops index several parallel arrays with one counter;
// clearer than zipped iterators.
#![allow(clippy::needless_range_loop)]
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

pub mod fft;
pub mod level;
pub mod loudness;
pub mod spectrum;
pub mod stereo;
pub mod transfer;
