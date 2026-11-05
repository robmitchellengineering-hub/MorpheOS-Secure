//! A planar ring buffer addressed by *absolute* sample index, whose recent
//! history can always be read as one contiguous slice.
//!
//! Every sample is written twice, at `i mod cap` and `i mod cap + cap`, so any
//! window of up to `cap` samples ending at or before the newest is contiguous.
//! That lets the interpolator read straight from the ring with no copying, no
//! allocation and no wrap-around branch.

use crate::prelude::*;

pub struct PlanarRing {
    cap: usize,
    channels: usize,
    /// `channels × 2·cap` samples.
    data: Vec<f32>,
    /// Absolute index one past the newest sample written.
    end: i64,
    /// Absolute index of the first sample ever written (the ring holds nothing
    /// before it).
    start: i64,
}

impl PlanarRing {
    pub fn new(channels: usize, cap: usize) -> PlanarRing {
        PlanarRing {
            cap,
            channels,
            data: vec![0.0; channels * cap * 2],
            end: 0,
            start: 0,
        }
    }

    /// Empties the ring and makes `first` the next index to be written.
    pub fn reset(&mut self, first: i64) {
        self.end = first;
        self.start = first;
    }

    /// One past the newest sample.
    pub fn end(&self) -> i64 {
        self.end
    }

    /// Oldest index still readable.
    pub fn oldest(&self) -> i64 {
        self.start.max(self.end - self.cap as i64)
    }

    /// Appends one frame (one sample per channel). No allocation.
    #[inline]
    pub fn push_frame(&mut self, frame: &[f32]) {
        let pos = self.end.rem_euclid(self.cap as i64) as usize;
        let lane = 2 * self.cap;
        for (c, &v) in frame.iter().take(self.channels).enumerate() {
            self.data[c * lane + pos] = v;
            self.data[c * lane + pos + self.cap] = v;
        }
        self.end += 1;
    }

    /// Adds `frame × weight` to the frame held at absolute `index` (both of its
    /// copies). Returns false if that frame is not held. No allocation.
    pub fn add(&mut self, index: i64, frame: &[f32], weight: f32) -> bool {
        if index < self.oldest() || index >= self.end {
            return false;
        }
        let pos = index.rem_euclid(self.cap as i64) as usize;
        let lane = 2 * self.cap;
        for (c, &v) in frame.iter().take(self.channels).enumerate() {
            self.data[c * lane + pos] += v * weight;
            self.data[c * lane + pos + self.cap] += v * weight;
        }
        true
    }

    /// `len` samples of channel `ch` starting at absolute index `first`, or
    /// `None` if any of them is not (or no longer) held.
    #[inline]
    pub fn window(&self, ch: usize, first: i64, len: usize) -> Option<&[f32]> {
        if len > self.cap || first < self.oldest() || first + len as i64 > self.end {
            return None;
        }
        let pos = first.rem_euclid(self.cap as i64) as usize;
        let base = ch * 2 * self.cap;
        Some(&self.data[base + pos..base + pos + len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_are_contiguous_across_the_wrap() {
        let mut r = PlanarRing::new(2, 8);
        r.reset(100);
        for i in 100..123 {
            r.push_frame(&[i as f32, -(i as f32)]);
        }
        // Newest 8 are 115..123; a window straddling the physical wrap point.
        let w = r.window(0, 116, 6).unwrap();
        assert_eq!(w, &[116.0, 117.0, 118.0, 119.0, 120.0, 121.0]);
        assert_eq!(r.window(1, 115, 8).unwrap()[0], -115.0);
    }

    #[test]
    fn stale_future_and_oversized_windows_are_refused() {
        let mut r = PlanarRing::new(1, 8);
        r.reset(0);
        for i in 0..20 {
            r.push_frame(&[i as f32]);
        }
        assert!(r.window(0, 11, 2).is_none(), "overwritten");
        assert!(r.window(0, 19, 2).is_none(), "not written yet");
        assert!(r.window(0, 12, 9).is_none(), "longer than the ring");
        assert!(r.window(0, 12, 8).is_some());
    }

    #[test]
    fn nothing_before_the_first_sample_is_readable() {
        let mut r = PlanarRing::new(1, 8);
        r.reset(50);
        r.push_frame(&[1.0]);
        assert!(r.window(0, 49, 2).is_none());
        assert_eq!(r.window(0, 50, 1).unwrap(), &[1.0]);
    }
}
