//! A node's rolling buffer of everything it sent (design 02, idea 6).
//!
//! The recorder has no deadline, so it can ask a node to resend any packet the
//! live path lost. This ring keeps the last `capacity` packets (with their
//! RTP sequence numbers), so a resend request can always be answered, and the
//! recording is bit-perfect 24-bit even where the live feed was not.

use crate::prelude::*;

use alloc::collections::VecDeque;

/// The last few packets a node sent, keyed by RTP sequence number.
pub struct RollingBuffer {
    capacity: usize,
    packets: VecDeque<(u16, Vec<u8>)>,
}

impl RollingBuffer {
    /// `capacity` packets (~60 s at 500 packets/s = 30 000).
    pub fn new(capacity: usize) -> RollingBuffer {
        RollingBuffer {
            capacity,
            packets: VecDeque::with_capacity(capacity),
        }
    }

    /// Remembers one sent packet. Evicts the oldest when full.
    pub fn push(&mut self, seq: u16, bytes: &[u8]) {
        if self.packets.len() == self.capacity {
            self.packets.pop_front();
        }
        self.packets.push_back((seq, bytes.to_vec()));
    }

    /// The packet with this sequence number, newest matching first (the u16
    /// wraps, so the newest of several matches is the one wanted).
    pub fn get(&self, seq: u16) -> Option<&[u8]> {
        self.packets
            .iter()
            .rev()
            .find(|(s, _)| *s == seq)
            .map(|(_, b)| b.as_slice())
    }

    pub fn len(&self) -> usize {
        self.packets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_keeps_packets_and_answers_by_sequence() {
        let mut b = RollingBuffer::new(4);
        b.push(1, b"one");
        b.push(2, b"two");
        assert_eq!(b.get(1), Some(&b"one"[..]));
        assert_eq!(b.get(2), Some(&b"two"[..]));
        assert_eq!(b.get(3), None);
    }

    #[test]
    fn it_evicts_the_oldest_when_full() {
        let mut b = RollingBuffer::new(3);
        for s in 1..=4 {
            b.push(s, b"x");
        }
        assert_eq!(b.len(), 3);
        assert_eq!(b.get(1), None, "the oldest is gone");
        assert_eq!(b.get(4), Some(&b"x"[..]));
    }

    #[test]
    fn a_wrapped_sequence_prefers_the_newest_match() {
        let mut b = RollingBuffer::new(70000);
        b.push(0, b"first");
        // The u16 wraps: the same sequence number appears again much later.
        b.push(1, b"middle");
        b.push(0, b"second");
        assert_eq!(b.get(0), Some(&b"second"[..]));
    }
}
