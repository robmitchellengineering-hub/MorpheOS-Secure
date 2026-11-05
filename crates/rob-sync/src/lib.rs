//! # rob-sync
//!
//! ROB-Sync (spec, "Clock sync"): a follower learns the leader's network time
//! from timestamped request/response exchanges, with no hardware timestamping.
//!
//! The pieces, in the order data flows through them:
//!
//! 1. [`filter`]: of each burst of exchanges keep the fastest (Wi-Fi queues only
//!    ever add delay, so the fastest is the most symmetric), then discard
//!    bursts that were slow compared with the recent window.
//! 2. [`servo`]: an alpha-beta tracker (a PI loop in another form) estimating the
//!    follower-to-leader offset and the relative frequency.
//! 3. [`follower`]: drives the filter and servo, and publishes a
//!    [`clock::ClockModel`] that never steps once locked: it slews toward the
//!    estimate at a bounded rate.
//! 4. [`clock`]: the model plus [`clock::SharedClock`], a lock-free cell the
//!    audio thread reads to convert local time to network time.
//!
//! [`election`] picks the leader when several are on offer, by clock class.
//!
//! [`net`] runs the leader and follower over UDP; [`sim`] simulates a network
//! for tests and tuning; [`replay`] feeds a recorded E1 probe trace through the
//! follower to see how it would have behaved on that real network. The leader's own clock is the network time: this crate
//! does not yet lock a leader to PTP.
//!
//! **no_std surface:** with `default-features = false` only [`stream`]'s
//! [`StreamClock`] (the receive-side map fitter) and [`inputclock`] are built —
//! the part a loadable Core program needs to follow a sender's clock without
//! sockets, threads or `std::time`.

#![cfg_attr(not(any(feature = "std", test)), no_std)]

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
    fn sqrt(self) -> Self;
    fn powi(self, e: i32) -> Self;
    fn rem_euclid(self, rhs: Self) -> Self;
    fn div_euclid(self, rhs: Self) -> Self;
    fn atan2(self, rhs: Self) -> Self;
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
    fn sqrt(self) -> f64 {
        libm::sqrt(self)
    }
    fn powi(self, e: i32) -> f64 {
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
    fn sqrt(self) -> f32 {
        libm::sqrtf(self)
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
}

/// The alloc items the crate's modules need unqualified. `use crate::prelude::*;`
/// is a glob import, so an unused member does not warn.
pub mod prelude {
    pub use super::FloatExt;
    pub use alloc::boxed::Box;
    pub use alloc::collections::VecDeque;
    pub use alloc::string::{String, ToString};
    pub use alloc::vec::Vec;
}

pub mod inputclock;
pub mod stream;

#[cfg(feature = "std")]
pub mod clock;
#[cfg(feature = "std")]
pub mod election;
#[cfg(feature = "std")]
pub mod filter;
#[cfg(feature = "std")]
pub mod follower;
#[cfg(feature = "std")]
pub mod net;
#[cfg(feature = "std")]
pub mod ptp;
#[cfg(feature = "std")]
pub mod replay;
#[cfg(feature = "std")]
pub mod servo;
#[cfg(feature = "std")]
pub mod sim;
