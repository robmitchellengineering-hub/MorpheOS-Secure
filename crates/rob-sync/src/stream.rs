//! Clock from the media stream itself (design 26): a receiver maps a
//! sender's timestamps to its own clock from the audio packets alone.
//!
//! Every packet gives one sample of `delta = arrival (receiver clock) -
//! sent (sender clock) = offset + one-way delay`. Delay is never below the
//! path's floor and is often far above it (stalls), so the **lower envelope**
//! of `delta` over time is `offset + floor`: a straight line whose slope is
//! the two clocks' frequency difference. Fit it from the fastest packet in
//! each short bucket, dropping buckets far above the line, over a sliding
//! window. At 500 packets per second there are 125x the samples two-way sync
//! gets, and no reverse path is needed.
//!
//! What it gives: where the sender's clock is, **up to a constant** (the
//! path's floor, unknown and irrelevant to playout, which adds its own
//! latency anyway). What it does not: alignment between receivers (that
//! constant differs per path) — two-way sync stays for that.

use crate::prelude::*;

#[derive(Debug, Clone, Copy)]
pub struct StreamClockConfig {
    /// One sample (the fastest packet) per bucket.
    pub bucket_ns: u64,
    /// Fit over this much recent history.
    pub window_ns: u64,
    /// Buckets this far above the fitted line (in robust sigmas) are stalls.
    pub reject_sigmas: f64,
    /// Buckets needed before the first estimate.
    pub min_buckets: usize,
}

impl Default for StreamClockConfig {
    fn default() -> Self {
        StreamClockConfig {
            bucket_ns: 250_000_000,
            window_ns: 60_000_000_000,
            reject_sigmas: 3.0,
            min_buckets: 12,
        }
    }
}

/// Minima beyond the line needed in a row to call a level shift (2 s).
const SHIFT_BUCKETS: usize = 8;
/// Smallest level shift recognised (ns): below this, the fit absorbs it.
const SHIFT_MIN_NS: f64 = 300_000.0;
/// The fit must span this much history before shifts are looked for (ns).
const MATURE_NS: f64 = 30e9;
/// With the rate held, the level comes from this many recent buckets (5 s).
const LEVEL_BUCKETS: usize = 20;
/// After a shift, keep the old rate until the new level spans this (ns).
const HOLD_RATE_NS: f64 = 20e9;
/// Start-up only: ridge-shrink the fitted rate toward 0 (the "we have not
/// seen enough history" prior) while the span is under `HOLD_RATE_NS`, so it
/// ramps up with history instead of holding at 0 ppm and then jumping. The
/// shrink is `sxx / (sxx + RATE_PRIOR_NS2)`: at ~3 s of buckets the rate is
/// ~3% of the fit, by `HOLD_RATE_NS` ~90% (design 26, "faster start").
const RATE_PRIOR_NS2: f64 = 3e20;
/// The widest real drift the start-up ridge may publish. Real crystals are
/// tens of ppm; 500 ppm is far beyond any of them, but bounds a degenerate
/// fit (a frozen sender) whose slope is enormous, so its `b · t0` cannot jump
/// the published clock backwards.
const RATE_MAX: f64 = 500e-6;

/// The fitted map: `delta(sent) ≈ a + b · (sent - t0)` (ns).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StreamMap {
    pub t0: f64,
    pub a: f64,
    pub b: f64,
}

impl StreamMap {
    /// Receiver-clock time at which a packet sent at `sent_ns` arrives at the
    /// path's floor.
    pub fn floor_arrival(&self, sent_ns: f64) -> f64 {
        sent_ns + self.a + self.b * (sent_ns - self.t0)
    }
    /// Sender-clock time at which a packet that arrives at `arrival_ns`
    /// (receiver clock) at the floor was sent: the inverse of
    /// [`floor_arrival`] (design 26, S4 — the engine needs it to unwrap a
    /// mono-stamped 32-bit RTP timestamp against the sender's clock).
    pub fn sender_time(&self, arrival_ns: f64) -> f64 {
        (arrival_ns - self.a + self.b * self.t0) / (1.0 + self.b)
    }
    /// The receiver's clock rate relative to the sender's, in ppm.
    pub fn ppm(&self) -> f64 {
        self.b * 1e6
    }
}

pub struct StreamClock {
    cfg: StreamClockConfig,
    /// Robust spread of the bucket minima about the last fit (ns).
    sigma: f64,
    /// Level shifts of the floor detected (a roam, a path or channel change).
    pub shifts: u64,
    /// After a shift: keep this rate until the new level has enough history.
    held_rate: Option<f64>,
    /// (bucket, sent ns, delta ns) for the fastest packet of each bucket.
    buckets: VecDeque<(u64, f64, f64)>,
    map: Option<StreamMap>,
}

impl StreamClock {
    pub fn new(cfg: StreamClockConfig) -> StreamClock {
        StreamClock {
            cfg,
            buckets: VecDeque::with_capacity(1024),
            map: None,
            sigma: 0.0,
            shifts: 0,
            held_rate: None,
        }
    }

    pub fn map(&self) -> Option<StreamMap> {
        self.map
    }

    /// The sender's clock reading now, given the receiver's clock reads
    /// `now_ns` (the inverse of the map). `None` until the first fit. The
    /// engine uses this as the reference to unwrap a mono-stamped 32-bit RTP
    /// timestamp (design 26, S4).
    pub fn sender_now(&self, now_ns: u64) -> Option<u64> {
        self.map
            .map(|m| m.sender_time(now_ns as f64).round().max(0.0) as u64)
    }

    /// One packet: sent at `sent_ns` (sender clock), arrived `arrival_ns`
    /// (receiver clock). Refits when a bucket completes.
    pub fn on_packet(&mut self, sent_ns: u64, arrival_ns: u64) {
        let delta = arrival_ns as f64 - sent_ns as f64;
        let k = arrival_ns / self.cfg.bucket_ns.max(1);
        match self.buckets.back_mut() {
            Some(b) if b.0 == k => {
                if delta < b.2 {
                    *b = (k, sent_ns as f64, delta);
                }
            }
            _ => {
                if self.buckets.back().is_some() {
                    self.refit();
                }
                self.buckets.push_back((k, sent_ns as f64, delta));
                let oldest = k.saturating_sub(self.cfg.window_ns / self.cfg.bucket_ns.max(1));
                while self.buckets.front().is_some_and(|b| b.0 < oldest) {
                    self.buckets.pop_front();
                }
            }
        }
    }

    /// Least-squares line through the completed buckets' minima, then again
    /// without those above it by more than `reject_sigmas` robust sigmas
    /// (stalls lift a bucket's minimum; nothing lowers it below the floor).
    fn refit(&mut self) {
        let n = self.buckets.len().saturating_sub(1); // the last one is still filling
        if n < self.cfg.min_buckets {
            return;
        }
        // A level shift: the last SHIFT_BUCKETS minima all beyond the current
        // line, all on the same side, by more than max(3 sigma, SHIFT_MIN_NS).
        // A stall lifts a few buckets at most, and only upwards; a changed
        // floor moves every bucket after it. Keep only the new level.
        // Only once the fit is established: while it is still settling, its
        // own error puts recent minima on one side of the line and would read
        // as a shift (then hold an immature rate).
        let span = match (self.buckets.front(), self.buckets.get(n.saturating_sub(1))) {
            (Some(f), Some(l)) => l.1 - f.1,
            _ => 0.0,
        };
        if let (Some(m), true) = (self.map, span >= MATURE_NS && self.held_rate.is_none()) {
            if n >= SHIFT_BUCKETS {
                let thr = (3.0 * self.sigma).max(SHIFT_MIN_NS);
                let recent: Vec<f64> = self
                    .buckets
                    .iter()
                    .take(n)
                    .skip(n - SHIFT_BUCKETS)
                    .map(|b| b.2 - (m.a + m.b * (b.1 - m.t0)))
                    .collect();
                let up = recent.iter().all(|r| *r > thr);
                let down = recent.iter().all(|r| *r < -thr);
                if up || down {
                    let drop = n - SHIFT_BUCKETS;
                    self.buckets.drain(..drop);
                    self.shifts += 1;
                    self.held_rate = Some(m.b);
                }
            }
        }
        let n = self.buckets.len().saturating_sub(1);
        let pts: Vec<(f64, f64)> = self.buckets.iter().take(n).map(|b| (b.1, b.2)).collect();
        if pts.len() < SHIFT_BUCKETS.min(self.cfg.min_buckets) {
            return;
        }
        let t0 = pts[0].0;
        // Right after a shift, a few seconds cannot fix a rate: keep the old
        // one (clocks do not change rate when a path changes) and fit only
        // the new level, until the new level spans HOLD_RATE_NS.
        let span = pts.last().map_or(0.0, |q| q.0) - t0;
        if let Some(b) = self.held_rate {
            if span < HOLD_RATE_NS {
                // Level from the most recent buckets only: with the rate held,
                // older ones are off by the rate error times their age.
                let recent = &pts[pts.len().saturating_sub(LEVEL_BUCKETS)..];
                let mut lv: Vec<f64> = recent.iter().map(|q| q.1 - b * (q.0 - t0)).collect();
                lv.sort_by(f64::total_cmp);
                // The lower part of the residuals is the floor.
                let a = lv[lv.len() / 4];
                self.map = Some(StreamMap { t0, a, b });
                return;
            }
            self.held_rate = None;
        }
        // Start-up (no shift yet): a few seconds cannot fix a rate, but holding
        // it at exactly 0 ppm walks the clock for a minute. Ridge-shrink the
        // rate toward 0 (the "not enough history yet" prior) so it ramps up
        // with data, and fit the level from the recent buckets only — a
        // full-span mean would let an old or degenerate floor (send timing
        // that is not a clock) poison the level the way it does in the
        // fallback's unusable-timing test.
        if span < HOLD_RATE_NS {
            let m = pts.len() as f64;
            let mx = pts.iter().map(|q| q.0 - t0).sum::<f64>() / m;
            let my = pts.iter().map(|q| q.1).sum::<f64>() / m;
            let sxx: f64 = pts.iter().map(|q| (q.0 - t0 - mx).powi(2)).sum();
            let sxy: f64 = pts.iter().map(|q| (q.0 - t0 - mx) * (q.1 - my)).sum();
            // The ridge only shrinks a *plausible* rate; degenerate send timing
            // (a frozen sender) fits a huge slope that the ridge alone will not
            // contain, so clamp to the widest real clock drift.
            let b = if sxx > 0.0 {
                (sxy / (sxx + RATE_PRIOR_NS2)).clamp(-RATE_MAX, RATE_MAX)
            } else {
                0.0
            };
            let recent = &pts[pts.len().saturating_sub(LEVEL_BUCKETS)..];
            let mut lv: Vec<f64> = recent.iter().map(|q| q.1 - b * (q.0 - t0)).collect();
            lv.sort_by(f64::total_cmp);
            let a = lv[lv.len() / 4];
            self.map = Some(StreamMap { t0, a, b });
            return;
        }
        let fit = |p: &[(f64, f64)]| -> Option<(f64, f64)> {
            let m = p.len() as f64;
            if m < 3.0 {
                return None;
            }
            let mx = p.iter().map(|q| q.0 - t0).sum::<f64>() / m;
            let my = p.iter().map(|q| q.1).sum::<f64>() / m;
            let sxx: f64 = p.iter().map(|q| (q.0 - t0 - mx).powi(2)).sum();
            let sxy: f64 = p.iter().map(|q| (q.0 - t0 - mx) * (q.1 - my)).sum();
            let b = if sxx > 0.0 { sxy / sxx } else { 0.0 };
            Some((my - b * mx, b))
        };
        let Some((mut a, mut b)) = fit(&pts) else {
            return;
        };
        for _ in 0..2 {
            let mut r: Vec<f64> = pts.iter().map(|q| q.1 - a - b * (q.0 - t0)).collect();
            let mut s = r.clone();
            s.sort_by(f64::total_cmp);
            let med = s[s.len() / 2];
            for v in s.iter_mut() {
                *v = (*v - med).abs();
            }
            s.sort_by(f64::total_cmp);
            let sigma = 1.4826 * s[s.len() / 2];
            self.sigma = sigma;
            let limit = med + self.cfg.reject_sigmas * sigma.max(10_000.0);
            let kept: Vec<(f64, f64)> = pts
                .iter()
                .zip(r.iter_mut())
                .filter(|(_, r)| **r <= limit)
                .map(|(p, _)| *p)
                .collect();
            if let Some((a2, b2)) = fit(&kept) {
                (a, b) = (a2, b2);
            }
        }
        self.map = Some(StreamMap { t0, a, b });
    }
}

/// Network time for a receiver that has no two-way sync: the sender's clock,
/// mapped from the stream (design 26, step S1). Feed it every packet of the
/// flow being played (network thread); read it anywhere (lock-free).
///
/// Network time here means **the sender's clock minus the path's floor**: a
/// packet at the floor arrives exactly at its send time, so the receiver's
/// latency covers jitter and its own pipeline, and the floor (a constant, a
/// few ms on Wi-Fi) is not counted in it.
///
/// Before the first fit (about 3 s) it uses the fastest packet so far. After,
/// it publishes the fitted map the way the two-way follower does: slewing at
/// most `SLEW_PPM` around the fitted rate, never jumping, unless the map
/// moves by more than `REANCHOR_NS` (a sender restart or a path change),
/// which re-anchors at once and is counted.
#[cfg(feature = "std")]
pub struct StreamTime {
    clock: crate::clock::SharedClock,
    inner: std::sync::Mutex<StreamTimeInner>,
}

#[cfg(feature = "std")]
struct StreamTimeInner {
    est: StreamClock,
    /// Lowest (arrival - sent) so far, before the first fit (ns).
    provisional: Option<f64>,
    last_map: Option<StreamMap>,
    pub reanchors: u64,
    /// Local times of recent re-anchors, so a caller can ask how *fast* they are
    /// coming. The count alone says a clock has moved; a rate says whether it is
    /// a clock at all (design 26, "unusable but publishing time").
    reanchor_times: VecDeque<u64>,
}

/// Largest map change slewed rather than re-anchored (ns).
#[cfg(feature = "std")]
pub const REANCHOR_NS: f64 = 20_000_000.0;
#[cfg(feature = "std")]
const SLEW_PPM: f64 = 200.0;
#[cfg(feature = "std")]
const HORIZON_NS: f64 = 8e9;

#[cfg(feature = "std")]
impl Default for StreamTime {
    fn default() -> Self {
        Self::new(StreamClockConfig::default())
    }
}

#[cfg(feature = "std")]
impl StreamTime {
    pub fn new(cfg: StreamClockConfig) -> StreamTime {
        StreamTime {
            clock: crate::clock::SharedClock::new(),
            inner: std::sync::Mutex::new(StreamTimeInner {
                est: StreamClock::new(cfg),
                provisional: None,
                last_map: None,
                reanchors: 0,
                reanchor_times: VecDeque::new(),
            }),
        }
    }

    /// Network time at local time `local_ns` (`None` before the first packet).
    pub fn now_at(&self, local_ns: u64) -> Option<u64> {
        self.clock.load().map(|m| m.net_ns(local_ns))
    }

    /// The fitted rate (receiver relative to sender, ppm) and re-anchors.
    pub fn status(&self) -> (Option<f64>, u64) {
        self.inner
            .lock()
            .map(|i| (i.last_map.map(|m| m.ppm()), i.reanchors))
            .unwrap_or((None, 0))
    }

    /// How many re-anchors happened within `window_ns` of local time
    /// `local_ns`. This is the signal that separates a clock that **moved** -
    /// a sender restart, a path change, one or two re-anchors and then real
    /// time again - from one that is **not a clock**: unusable send timing
    /// re-anchors about four times a second and keeps doing it (design 26,
    /// "the failure modes, enumerated").
    pub fn reanchors_within(&self, local_ns: u64, window_ns: u64) -> usize {
        let Ok(i) = self.inner.lock() else { return 0 };
        let from = local_ns.saturating_sub(window_ns);
        i.reanchor_times.iter().filter(|&&t| t >= from).count()
    }

    /// One packet of the played flow: sent at `sent_ns` (sender clock),
    /// arrived at local time `arrival_ns`.
    pub fn feed(&self, sent_ns: u64, arrival_ns: u64) {
        let Ok(mut i) = self.inner.lock() else { return };
        i.est.on_packet(sent_ns, arrival_ns);
        let map = i.est.map();
        match map {
            None => {
                // Provisional: as if this packet (if the fastest yet) arrived
                // at the floor.
                let d = arrival_ns as f64 - sent_ns as f64;
                if i.provisional.is_none_or_lower(d) {
                    i.provisional = Some(d);
                    self.clock.store(crate::clock::ClockModel::line(
                        arrival_ns,
                        (arrival_ns as f64 - d).max(0.0) as u64,
                        0.0,
                    ));
                }
            }
            Some(m) if Some(m) != i.last_map => {
                i.last_map = Some(m);
                // Sender time at local time L: invert floor_arrival.
                let target = |l: f64| (l - m.a + m.b * m.t0) / (1.0 + m.b);
                let rate = (1.0 / (1.0 + m.b) - 1.0) * 1e6;
                let l = arrival_ns;
                let want = target(l as f64);
                let have = self.clock.load().map(|c| c.net_ns(l) as f64);
                let model = match have {
                    Some(y0) if (want - y0).abs() <= REANCHOR_NS => {
                        let err = want - y0;
                        let horizon = HORIZON_NS.max(err.abs() / (SLEW_PPM * 1e-6));
                        let r =
                            ((err / horizon) * 1e6 + rate).clamp(rate - SLEW_PPM, rate + SLEW_PPM);
                        crate::clock::ClockModel {
                            local_ref_ns: l,
                            net_ref_ns: y0.max(0.0) as u64,
                            rate_ppm: r,
                            knee_ns: l.saturating_add(horizon as u64),
                            rate2_ppm: rate,
                        }
                    }
                    _ => {
                        if have.is_some() {
                            i.reanchors += 1;
                            // Keep a minute of them: enough for any window a
                            // caller asks about, bounded so it cannot grow.
                            i.reanchor_times.push_back(l);
                            let oldest = l.saturating_sub(60_000_000_000);
                            while i.reanchor_times.front().is_some_and(|&t| t < oldest) {
                                i.reanchor_times.pop_front();
                            }
                        }
                        crate::clock::ClockModel::line(l, want.max(0.0) as u64, rate)
                    }
                };
                self.clock.store(model);
            }
            Some(_) => {}
        }
    }
}

#[cfg(feature = "std")]
impl crate::clock::TimeSource for StreamTime {
    fn now_ns(&self) -> u64 {
        self.now_at(crate::clock::mono_ns()).unwrap_or(0)
    }
}

#[cfg(feature = "std")]
trait LowerOpt {
    fn is_none_or_lower(&self, d: f64) -> bool;
}
#[cfg(feature = "std")]
impl LowerOpt for Option<f64> {
    fn is_none_or_lower(&self, d: f64) -> bool {
        self.map_or(true, |p| d < p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 120 s at 500 packets/s: receiver 1 s ahead and 40 ppm fast, a 2 ms
    /// floor, small jitter, and a 300 ms stall every 5 s (packets held, then
    /// dumped back to back).
    #[test]
    fn finds_the_sender_clock_through_stalls() {
        let mut c = StreamClock::new(StreamClockConfig::default());
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut worst = 0.0f64;
        for n in 0..60_000u64 {
            let sent = 5_000_000_000 + n * 2_000_000;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let jitter = (x % 400_000) as f64;
            let in_cycle = n % 2_500;
            let stall = if in_cycle < 150 {
                (300.0 - 2.0 * in_cycle as f64) * 1e6
            } else {
                0.0
            };
            let truth = |s: f64| 1e9 + s * (1.0 + 40e-6) + 2e6;
            let arrival = truth(sent as f64) + jitter + stall;
            c.on_packet(sent, arrival as u64);
            if n > 20_000 && n % 500 == 0 {
                let m = c.map().unwrap_or_else(|| panic!("no map at {n}"));
                worst = worst.max((m.floor_arrival(sent as f64) - truth(sent as f64)).abs());
            }
        }
        let m = c.map().unwrap_or_else(|| panic!("no map"));
        assert!((m.ppm() - 40.0).abs() < 0.5, "{} ppm", m.ppm());
        assert!(worst < 200_000.0, "worst {worst} ns from the floor line");
    }

    /// `sender_time` is the inverse of `floor_arrival` (design 26, S4): the
    /// engine unwraps a mono-stamped 32-bit timestamp against it, so a wrong
    /// formula would place packets in the wrong period.
    #[test]
    fn stream_map_inverse_round_trips() {
        let m = StreamMap {
            t0: 1.0e9,
            a: 2.0e6,
            b: 40e-6,
        };
        for x in [0.0f64, 1.0e9, 5.0e9, 123.6e9, 1.0e12] {
            let y = m.floor_arrival(x);
            assert!(
                (m.sender_time(y) - x).abs() < 0.01,
                "sender_time(floor_arrival({x})) = {}, not {x}",
                m.sender_time(y)
            );
        }
    }

    #[test]
    fn stream_time_tracks_the_sender_and_never_jumps() {
        // Receiver local clock: 3 s ahead and 50 ppm slow of the sender's.
        // Floor 2 ms, jitter up to 0.4 ms, a 300 ms stall every 5 s.
        let st = StreamTime::default();
        let local = |s: f64| 3e9 + s * (1.0 - 50e-6);
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let (mut last_net, mut last_local) = (None::<u64>, 0u64);
        let mut worst_tail = 0.0f64;
        let mut biggest_step = 0.0f64;
        for n in 0..60_000u64 {
            let sent = 10_000_000_000 + n * 2_000_000;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let in_cycle = n % 2_500;
            let stall = if in_cycle < 150 {
                (300.0 - 2.0 * in_cycle as f64) * 1e6
            } else {
                0.0
            };
            let arrive_true = sent as f64 + 2e6 + (x % 400_000) as f64 + stall;
            let arrival = local(arrive_true) as u64;
            st.feed(sent, arrival);
            // Read the clock every 10 ms of local time: no step beyond what
            // 10 ms at the rate plus the slew allows (a jump would show).
            if arrival >= last_local + 10_000_000 {
                if let (Some(prev), Some(now)) = (last_net, st.now_at(arrival)) {
                    if n > 2_000 {
                        let dl = (arrival - last_local) as f64;
                        let step = (now as f64 - prev as f64 - dl).abs() - dl * 300e-6;
                        biggest_step = biggest_step.max(step);
                    }
                }
                last_net = st.now_at(arrival);
                last_local = arrival;
            }
            // After 60 s: network time = sender time minus the floor, tight.
            if n > 30_000 && n % 500 == 0 {
                let net = st.now_at(arrival).unwrap_or(0) as f64;
                // At this local moment the sender's clock reads:
                let sender_now = (arrival as f64 - 3e9) / (1.0 - 50e-6);
                worst_tail = worst_tail.max((net - (sender_now - 2e6)).abs());
            }
        }
        let (ppm, reanchors) = st.status();
        assert!(ppm.is_some_and(|p| (p + 50.0).abs() < 1.0), "{ppm:?}");
        assert_eq!(reanchors, 0);
        assert!(
            worst_tail < 300_000.0,
            "worst {worst_tail} ns from sender time minus floor"
        );
        assert!(biggest_step < 50_000.0, "a step of {biggest_step} ns");
    }

    #[test]
    fn a_floor_step_is_followed_within_seconds_and_stalls_are_not_shifts() {
        // 150 s: receiver 40 ppm fast; floor 2 ms, then +1.5 ms at 60 s,
        // then -1.0 ms at 100 s; 300 ms stalls every 5 s throughout.
        let mut c = StreamClock::new(StreamClockConfig::default());
        let mut x = 0x1234_5678_9ABC_DEF1u64;
        let floor = |t: f64| {
            2e6 + if t >= 100e9 {
                0.5e6
            } else if t >= 60e9 {
                1.5e6
            } else {
                0.0
            }
        };
        let mut worst_after = 0.0f64;
        for n in 0..75_000u64 {
            let sent = n * 2_000_000;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let in_cycle = n % 2_500;
            let stall = if in_cycle < 150 {
                (300.0 - 2.0 * in_cycle as f64) * 1e6
            } else {
                0.0
            };
            let t = sent as f64;
            let truth_floor = 1e9 + t * (1.0 + 40e-6) + floor(t);
            let arrival = truth_floor + (x % 400_000) as f64 + stall;
            c.on_packet(sent, arrival as u64);
            // 5 s after each step, the map must sit on the new floor.
            let settled = (t > 65e9 && t < 100e9) || t > 105e9;
            if settled && n % 250 == 0 {
                let m = c.map().unwrap_or_else(|| panic!("no map"));
                worst_after = worst_after.max((m.floor_arrival(t) - truth_floor).abs());
            }
        }
        assert_eq!(c.shifts, 2, "two real steps, and no stall taken for one");
        assert!(
            worst_after < 250_000.0,
            "worst {worst_after} ns off the new floor"
        );
    }
}
