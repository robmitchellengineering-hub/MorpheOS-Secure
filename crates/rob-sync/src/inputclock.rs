//! An engine input's clock (design 26, step S4): where a sender's timestamp
//! falls on the **engine's** time, from the sender's own stream plus one slowly
//! learned constant.
//!
//! The stream clock ([`crate::stream`]) gives a line that is steady to tens of
//! microseconds, but only **up to a constant**: it tracks `engine time + the
//! path's floor`, and the floor (a few ms on Wi-Fi) is different for every
//! input. Playout does not care. **Alignment between inputs does**: two mics on
//! different nodes hearing the same source are lined up by the engine, so a
//! 3 ms difference between their floors is a 3 ms error.
//!
//! The constant is what two-way sync is good at, if it is averaged: each
//! calibration sample says "at this sender clock reading the engine's time was
//! about E" and is noisy (hundreds of microseconds on Wi-Fi). The floor is
//! `stream line at that reading - E`; a trimmed mean over the stream clock's own
//! window shrinks the noise by the square root of the sample count, and being a
//! constant it needs no fast tracking. A stall or a holdover in two-way sync
//! therefore no longer touches the audio's timing; it only delays a refresh of
//! the constant.
//!
//! * **Samples are re-evaluated against the current stream line** every time,
//!   not frozen when taken. The line is refitted all the time and shifts level
//!   when the path changes (a roam): the floor is then read off the *current*
//!   line, so a level shift needs no special case here.
//! * **Window = the stream clock's window (60 s) by default: a hypothesis.**
//!   Extrapolating the fitted line further back multiplies its rate error
//!   (0.3 ppm over 5 minutes is 90 us), which argues for a short window; more
//!   samples average the noise down, which argues for a long one. The
//!   simulation (constant-drift clocks, so it cannot see the first effect)
//!   measures only the second: worst error 5 to 10 minutes in was 55 us with
//!   60 s and 24 us with 300 s. **Real clocks' drift wanders with temperature and
//!   is not simulated, so the default stays at 60 s until a real pair of clocks
//!   says otherwise** (an E3-style run: two nodes, an hour).
//! * **Limits (stated, not hidden):** the constant is only as good as the
//!   symmetry of the path that two-way sync assumes; a path whose up and down
//!   delays differ by `d` leaves `d/2` of error that no amount of averaging
//!   removes (E3 measures it). And **how a node's two-way estimate reaches the
//!   engine is not decided here**: the engine sees only requests, the node sees
//!   the round trip, so a calibration sample must be sent (a wire change, Rob's
//!   decision) or the node must stamp in network time as today.

use crate::prelude::*;
use crate::stream::{StreamClock, StreamClockConfig};

#[derive(Debug, Clone, Copy)]
pub struct InputClockConfig {
    pub stream: StreamClockConfig,
    /// Calibration samples older than this are dropped (ns of engine time).
    pub window_ns: u64,
    /// Samples needed before the constant is trusted.
    pub min_samples: usize,
    /// Fraction trimmed from each end before averaging (outliers are bursts,
    /// which only ever make a sample late).
    pub trim: f64,
}

impl Default for InputClockConfig {
    fn default() -> Self {
        InputClockConfig {
            stream: StreamClockConfig::default(),
            window_ns: 60_000_000_000,
            min_samples: 30,
            trim: 0.25,
        }
    }
}

/// An input's sender timestamp placed on engine time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mapped {
    /// Engine time of the send (ns).
    pub engine_ns: f64,
    /// False while the constant is not yet learned: `engine_ns` then includes
    /// this input's floor and must not be used to line inputs up.
    pub calibrated: bool,
}

pub struct InputClock {
    cfg: InputClockConfig,
    clock: StreamClock,
    /// (sender clock reading, engine time then): two-way sync samples.
    calib: VecDeque<(f64, f64)>,
    floor_ns: Option<f64>,
    /// Lowest (arrival - sent) seen so far: the provisional offset used before
    /// the stream clock's first fit, so the engine plays from the first packet
    /// instead of dropping the first ~3 s (design 26, S4).
    provisional: Option<f64>,
}

impl InputClock {
    pub fn new(cfg: InputClockConfig) -> InputClock {
        InputClock {
            cfg,
            clock: StreamClock::new(cfg.stream),
            calib: VecDeque::new(),
            floor_ns: None,
            provisional: None,
        }
    }

    /// One packet of this input: sent at `sent_ns` (sender clock), arrived at
    /// `arrival_ns` (engine clock).
    pub fn on_packet(&mut self, sent_ns: u64, arrival_ns: u64) {
        let delta = arrival_ns as f64 - sent_ns as f64;
        self.provisional = Some(self.provisional.map_or(delta, |p| p.min(delta)));
        self.clock.on_packet(sent_ns, arrival_ns);
    }

    /// The sender's clock reading at engine time `engine_ns`: the reference
    /// the engine unwraps this input's mono-stamped 32-bit RTP timestamp
    /// against (design 26, S4). The stream clock's inverse lives in the
    /// RTP-wrapped domain (the 32-bit wrap offset is lost), so a two-way
    /// calibration sample anchors it to the sender's real clock: at sender
    /// clock `s_real` the engine time was `e`, so a rate-1 extrapolation
    /// recovers the wrap — and a rate-1 error is tiny against the unwrap's
    /// ~12-hour tolerance. `None` until the first calibration sample.
    pub fn sender_now(&self, engine_ns: u64) -> Option<u64> {
        let (s_real, e) = *self.calib.back()?;
        Some((s_real + (engine_ns as f64 - e)) as u64)
    }

    /// One two-way sync result: when the sender's clock read `sender_ns`, the
    /// engine's time was about `engine_ns`. Averaged, not trusted singly.
    pub fn on_calibration(&mut self, sender_ns: u64, engine_ns: u64) {
        self.calib.push_back((sender_ns as f64, engine_ns as f64));
        let oldest = engine_ns.saturating_sub(self.cfg.window_ns) as f64;
        while self.calib.front().is_some_and(|c| c.1 < oldest) {
            self.calib.pop_front();
        }
    }

    /// Recompute the constant against the current stream line. Call about
    /// once a second from the network thread (it is O(window)).
    pub fn refresh(&mut self) {
        let Some(map) = self.clock.map() else {
            self.floor_ns = None;
            return;
        };
        if self.calib.len() < self.cfg.min_samples {
            self.floor_ns = None;
            return;
        }
        let mut f: Vec<f64> = self
            .calib
            .iter()
            .map(|&(s, e)| map.floor_arrival(s) - e)
            .collect();
        f.sort_by(f64::total_cmp);
        let cut = ((f.len() as f64 * self.cfg.trim) as usize).min(f.len().saturating_sub(1) / 2);
        let kept = &f[cut..f.len() - cut];
        self.floor_ns = Some(kept.iter().sum::<f64>() / kept.len() as f64);
    }

    /// Engine time at which the sender sent the packet stamped `sent_ns`.
    /// Before the stream clock's first fit it maps from the lowest delta seen
    /// so far (provisional), so the first packets play instead of being
    /// dropped; `calibrated` is false either way until the constant is learned.
    pub fn engine_time(&self, sent_ns: u64) -> Option<Mapped> {
        let Some(map) = self.clock.map() else {
            return self.provisional.map(|d| Mapped {
                engine_ns: sent_ns as f64 + d,
                calibrated: false,
            });
        };
        let line = map.floor_arrival(sent_ns as f64);
        Some(match self.floor_ns {
            Some(f) => Mapped {
                engine_ns: line - f,
                calibrated: true,
            },
            None => Mapped {
                engine_ns: line,
                calibrated: false,
            },
        })
    }

    /// The learned floor (ns), the calibration samples held, and the stream
    /// clock's level shifts so far (for the desk's Network panel, S5).
    pub fn status(&self) -> (Option<f64>, usize, u64) {
        (self.floor_ns, self.calib.len(), self.clock.shifts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: f64 = 1e6;
    const S: f64 = 1e9;

    /// One simulated node: its clock reads `offset + (1 + drift) * t` at true
    /// (engine) time `t`; its packets cross a path with a fixed `floor`, a
    /// little jitter and, optionally, a stall.
    #[derive(Clone, Copy)]
    struct Node {
        offset_ns: f64,
        drift_ppm: f64,
        floor_ns: f64,
        rng: u64,
    }

    impl Node {
        fn clock(&self, t: f64) -> f64 {
            self.offset_ns + t * (1.0 + self.drift_ppm * 1e-6)
        }
        fn rand(&mut self) -> f64 {
            self.rng ^= self.rng << 13;
            self.rng ^= self.rng >> 7;
            self.rng ^= self.rng << 17;
            (self.rng >> 11) as f64 / (1u64 << 53) as f64
        }
        /// Approximately normal, mean 0, sd 1 (sum of 12 uniforms).
        fn gauss(&mut self) -> f64 {
            (0..12).map(|_| self.rand()).sum::<f64>() - 6.0
        }
    }

    /// Drives one node's packets (500/s) and two-way samples (1/s) into `ic`
    /// for true times in `[from, to)` seconds. `stall`: packets sent in this
    /// window (s) are held and arrive together at its end, as a Wi-Fi stall
    /// does. Two-way samples have 100 us of noise and 3% late outliers.
    fn drive(ic: &mut InputClock, n: &mut Node, from: u64, to: u64, stall: Option<(f64, f64)>) {
        for k in (from * 500)..(to * 500) {
            let t = k as f64 * 2.0 * MS;
            let mut arrive = t + n.floor_ns + n.rand() * 0.3 * MS;
            if let Some((a, b)) = stall {
                if t >= a * S && t < b * S {
                    arrive = arrive.max(b * S + n.rand() * MS);
                }
            }
            ic.on_packet(n.clock(t) as u64, arrive as u64);
            if k % 500 == 250 {
                let mut e = t + n.gauss() * 100e3;
                if n.rand() < 0.03 {
                    e += (1.0 + 2.0 * n.rand()) * MS;
                }
                ic.on_calibration(n.clock(t) as u64, e as u64);
            }
            if k % 500 == 499 {
                ic.refresh();
            }
        }
    }

    fn two_nodes() -> (Node, Node) {
        (
            Node {
                offset_ns: 3.7 * S,
                drift_ppm: 30.0,
                floor_ns: 2.0 * MS,
                rng: 0x9E37_79B9_7F4A_7C15,
            },
            Node {
                offset_ns: 12.4 * S,
                drift_ppm: -50.0,
                floor_ns: 5.0 * MS,
                rng: 0x2545_F491_4F6C_DD1D,
            },
        )
    }

    /// Both nodes hear the same instant `t`; the engine's two placements of it
    /// differ by this much (ns).
    fn alignment_error(a: (&InputClock, &Node), b: (&InputClock, &Node), t: f64) -> f64 {
        let ta = a.0.engine_time(a.1.clock(t) as u64).unwrap();
        let tb = b.0.engine_time(b.1.clock(t) as u64).unwrap();
        assert!(
            ta.calibrated && tb.calibrated,
            "not calibrated at t={}",
            t / S
        );
        ta.engine_ns - tb.engine_ns
    }

    /// The S4 done-when: two nodes, different floors (2 and 5 ms) and drifts
    /// (+30 and -50 ppm), and a 300 ms stall on one of them: after 2 minutes the
    /// engine lines their inputs up to under 100 us, and stays there.
    #[test]
    fn two_nodes_with_different_floors_and_drifts_line_up_under_100_us_through_a_stall() {
        let (mut n1, mut n2) = two_nodes();
        let (mut a, mut b) = (
            InputClock::new(InputClockConfig::default()),
            InputClock::new(InputClockConfig::default()),
        );
        drive(&mut a, &mut n1, 0, 120, None);
        drive(&mut b, &mut n2, 0, 120, Some((100.0, 100.3)));
        let mut worst = 0.0f64;
        // Keep running 60 s more, with a second stall on node 1, measuring each second.
        for sec in 120..180u64 {
            drive(&mut a, &mut n1, sec, sec + 1, Some((150.0, 150.3)));
            drive(&mut b, &mut n2, sec, sec + 1, None);
            let t = (sec as f64 + 0.9) * S;
            worst = worst.max(alignment_error((&a, &n1), (&b, &n2), t).abs());
        }
        assert!(worst < 100e3, "worst alignment error {:.1} us", worst / 1e3);
    }

    /// Without the learned constant the engine is wrong by the difference of
    /// the floors (3 ms here): the calibration is what S4 adds.
    #[test]
    fn without_the_constant_inputs_are_off_by_the_difference_of_their_floors() {
        let (mut n1, mut n2) = two_nodes();
        let cfg = InputClockConfig {
            min_samples: usize::MAX, // never calibrates
            ..InputClockConfig::default()
        };
        let (mut a, mut b) = (InputClock::new(cfg), InputClock::new(cfg));
        drive(&mut a, &mut n1, 0, 130, None);
        drive(&mut b, &mut n2, 0, 130, None);
        let (ta, tb) = (
            a.engine_time(n1.clock(125.0 * S) as u64).unwrap(),
            b.engine_time(n2.clock(125.0 * S) as u64).unwrap(),
        );
        assert!(!ta.calibrated && !tb.calibrated);
        let err = (ta.engine_ns - tb.engine_ns).abs();
        assert!((2.5 * MS..3.5 * MS).contains(&err), "{:.0} us", err / 1e3);
    }

    /// Not calibrated until enough samples: it says so rather than guessing.
    #[test]
    fn it_reports_uncalibrated_until_enough_samples() {
        let (mut n1, _) = two_nodes();
        let mut a = InputClock::new(InputClockConfig::default());
        drive(&mut a, &mut n1, 0, 20, None); // 20 samples < 30
        assert!(!a.engine_time(n1.clock(19.0 * S) as u64).unwrap().calibrated);
        drive(&mut a, &mut n1, 20, 40, None);
        assert!(a.engine_time(n1.clock(39.0 * S) as u64).unwrap().calibrated);
    }

    /// The engine unwraps a mono-stamped 32-bit RTP timestamp against the
    /// sender's own clock; `sender_now` is that reference. At engine time `t`
    /// the sender's clock reads `clock(t)`, so the reference must track it.
    /// The rate-1 estimate carries the calibration sample's 100 us of noise
    /// and its 3% late outliers, hence the 5 ms bound; a wrap-domain or
    /// floor-domain regression is off by ~1e15 ns or the 2 ms floor.
    #[test]
    fn sender_now_inverts_the_map_to_the_senders_clock() {
        let (mut n1, _) = two_nodes();
        let mut a = InputClock::new(InputClockConfig::default());
        drive(&mut a, &mut n1, 0, 120, None);
        let t = 119.9 * S;
        let got = a
            .sender_now(t as u64)
            .expect("a calibration sample anchors the reference");
        let want = n1.clock(t);
        assert!(
            (got as f64 - want).abs() < 5_000_000.0,
            "sender_now {got} ns vs the sender's {want:.0} ns"
        );
    }

    /// The 32-bit RTP timestamp wraps every ~24.8 h, so a sender that has been
    /// up for days stamps a small residue while its real clock is ~1e15 ns.
    /// The engine's unwrap reference must be the REAL clock, recovered from a
    /// two-way calibration sample, or every packet unwraps to the residue and
    /// the input is concealed (the sender->engine bug found 2026-10-19).
    #[test]
    fn sender_now_recovers_the_rtp_wrap_from_a_calibration_sample() {
        let n = Node {
            offset_ns: 1_000_000_000_000_000.0, // ~11.6 days up: the RTP has wrapped
            drift_ppm: 0.0,
            floor_ns: 2.0 * MS,
            rng: 0x1234_5678,
        };
        let mut ic = InputClock::new(InputClockConfig::default());
        // The engine sees the WRAPPED timestamp (clock mod 2^32 samples) before
        // the fix; the calibration carries the real clock.
        let wrap_ns = (1u64 << 32) as f64 * 1e9 / 48_000.0;
        let sec = 5u64;
        for k in 0..(sec * 500) {
            let t = k as f64 * 2.0 * MS;
            let full = n.clock(t);
            let wrapped = full.rem_euclid(wrap_ns);
            ic.on_packet(wrapped as u64, (t + n.floor_ns) as u64);
        }
        ic.on_calibration(n.clock(4.0 * S) as u64, (4.0 * S) as u64);
        ic.refresh();
        let t = 4.5 * S;
        let got = ic
            .sender_now(t as u64)
            .expect("a calibration sample anchors the reference");
        let want = n.clock(t);
        assert!(
            (got as f64 - want).abs() < 2.0e6,
            "sender_now {got} ns vs the sender's real {want:.0} ns"
        );
    }

    /// Before the stream clock's first fit (~3 s), the engine still maps from
    /// the first packet using the lowest delta seen so far — it must not drop
    /// the first 3 s (design 26, S4).
    #[test]
    fn it_maps_provisionally_before_the_stream_clock_fits() {
        let (mut n1, _) = two_nodes();
        let mut a = InputClock::new(InputClockConfig::default());
        drive(&mut a, &mut n1, 0, 2, None); // 2 s < the ~3 s fit
        let t = 1.9 * S;
        let m = a
            .engine_time(n1.clock(t) as u64)
            .expect("provisional mapping before the fit");
        assert!(!m.calibrated);
        // Provisional = arrival-at-floor = true time + floor, within the
        // drive's 0..0.3 ms jitter.
        let expected = t + n1.floor_ns;
        assert!(
            (m.engine_ns - expected).abs() < 1.0e6,
            "provisional {} vs {expected}",
            m.engine_ns
        );
    }

    /// A roam moves one input's floor by 1.5 ms. The constant is read off the
    /// current line, so within a stream-clock window the two inputs line up
    /// again with no special handling.
    #[test]
    fn a_floor_step_on_one_input_is_followed() {
        let (mut n1, mut n2) = two_nodes();
        let (mut a, mut b) = (
            InputClock::new(InputClockConfig::default()),
            InputClock::new(InputClockConfig::default()),
        );
        drive(&mut a, &mut n1, 0, 150, None);
        drive(&mut b, &mut n2, 0, 150, None);
        n2.floor_ns += 1.5 * MS;
        let mut errs = Vec::new();
        for sec in 150..260u64 {
            drive(&mut a, &mut n1, sec, sec + 1, None);
            drive(&mut b, &mut n2, sec, sec + 1, None);
            errs.push((
                sec,
                alignment_error((&a, &n1), (&b, &n2), (sec as f64 + 0.9) * S).abs(),
            ));
        }
        let late: f64 = errs
            .iter()
            .filter(|e| e.0 >= 230)
            .map(|e| e.1)
            .fold(0.0, f64::max);
        assert!(
            late < 100e3,
            "30 s after settling: {:.1} us ({errs:?})",
            late / 1e3
        );
    }

    /// Ten minutes, so a calibration window longer than the stream clock's own
    /// has samples to reach back over. Returns the worst alignment error (ns)
    /// between 5 and 10 minutes.
    fn ten_minutes(window_ns: u64) -> f64 {
        let (mut n1, mut n2) = two_nodes();
        let cfg = InputClockConfig {
            window_ns,
            ..InputClockConfig::default()
        };
        let (mut a, mut b) = (InputClock::new(cfg), InputClock::new(cfg));
        let mut worst = 0.0f64;
        for sec in 0..600u64 {
            drive(&mut a, &mut n1, sec, sec + 1, None);
            drive(&mut b, &mut n2, sec, sec + 1, None);
            if sec >= 300 {
                let t = (sec as f64 + 0.9) * S;
                worst = worst.max(alignment_error((&a, &n1), (&b, &n2), t).abs());
            }
        }
        worst
    }

    /// Stays aligned for ten minutes. The window comparison is reported, not
    /// asserted: on constant-drift clocks a longer window is better (more
    /// samples), the opposite of the worry behind the 60 s default, and this
    /// simulation cannot show that worry (drift that wanders).
    #[test]
    fn it_stays_aligned_for_ten_minutes() {
        let w60 = ten_minutes(60_000_000_000);
        let w300 = ten_minutes(300_000_000_000);
        eprintln!(
            "worst error 5-10 min: 60 s window {:.1} us, 300 s window {:.1} us",
            w60 / 1e3,
            w300 / 1e3
        );
        assert!(w60 < 100e3, "60 s window: {:.1} us", w60 / 1e3);
        assert!(w300 < 100e3, "300 s window: {:.1} us", w300 / 1e3);
    }

    /// The stated limit, as a test: if one input's path is asymmetric by 1 ms
    /// (up slower than down), two-way sync's estimate is wrong by half of it
    /// and averaging cannot remove that. The engine's alignment is off by that
    /// 0.5 ms. This is why E3 (the click test) measures asymmetry, and why the
    /// "under 100 us" claim is for symmetric paths only.
    #[test]
    fn path_asymmetry_is_a_floor_that_averaging_cannot_remove() {
        let (mut n1, n2) = two_nodes();
        let (mut a, mut b) = (
            InputClock::new(InputClockConfig::default()),
            InputClock::new(InputClockConfig::default()),
        );
        drive(&mut a, &mut n1, 0, 150, None);
        // Node 2's calibration samples all arrive 0.5 ms late (half of 1 ms).
        let mut biased = Node { rng: n2.rng, ..n2 };
        for sec in 0..150u64 {
            drive(&mut b, &mut biased, sec, sec + 1, None);
            // drive() already added a sample at 250 ms; add the bias by
            // replacing the newest sample.
            if let Some(last) = b.calib.back_mut() {
                last.1 += 0.5 * MS;
            }
        }
        b.refresh();
        let err = alignment_error((&a, &n1), (&b, &biased), 149.9 * S).abs();
        assert!((0.4 * MS..0.6 * MS).contains(&err), "{:.0} us", err / 1e3);
    }
}
