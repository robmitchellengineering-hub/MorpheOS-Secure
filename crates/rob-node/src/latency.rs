//! Automatic latency for a receiver: lower it while every period arrives with
//! room to spare, raise it at once when one does not.
//!
//! The receiver measures, for every period, how much earlier than needed its
//! full-quality copy arrived (its *headroom*, see `ReceiverAudio::headroom`).
//! After a quiet window with no late period, the latency is lowered by half
//! the spare headroom (smallest seen, minus a safety margin), so it eases
//! towards the floor rather than overshooting. A late period raises it by the
//! margin straight away and is remembered: the latency never again goes below
//! where a period was late plus the margin. A short window cannot see a
//! network's rare worst moments; that memory is what stops it hunting.
//!
//! **Outages are not lateness.** A roam between access points (or a radio
//! dropping off) stops arrivals for tens to hundreds of milliseconds, and the
//! packets queued meanwhile then arrive late in a rush. No latency short of
//! the outage would have saved them, so raising (and remembering) would only
//! ratchet the latency up a notch per roam. After an outage the controller
//! ignores lateness and headroom for a few seconds instead. **Unless outages
//! are routine:** on a bursty link (Rob's Wi-Fi, 2026-10-10: gaps of 20-600
//! ms many times a minute) they are the link's normal behaviour, not roams,
//! and excusing them stopped it ever raising. Several in a minute and they
//! count as lateness.
//!
//! **Raises are sized** by how late the copy was (negative headroom), plus
//! the margin, not a millimetre at a time. **A ceiling** (a show setting)
//! caps it: on one radio path some links have no clean latency at any live
//! value (field data: none up to 200 ms), so instead of climbing forever it
//! stops and says so (`over_ceiling`): that link needs a second path.
//!
//! Changes are never jumps: the receiver slews its latency at a small rate
//! (parts per million of playback speed, an inaudible pitch change far below
//! the ~5 cent threshold), so the phase-locked resampler follows it like a
//! slightly fast or slow clock. Pure logic here; the slew lives in the
//! receiver. No allocation.

/// Settings for automatic latency.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoConfig {
    /// Headroom kept in reserve (ns).
    pub margin_ns: u64,
    /// Clean seconds needed before lowering.
    pub window_s: u32,
    /// Never below / above these (ns).
    pub floor_ns: u64,
    pub ceiling_ns: u64,
    /// Slew rates as a fraction of playback speed (1e-6 = 1 ppm).
    pub down_rate: f64,
    pub up_rate: f64,
    /// Lower only when it would gain at least this much (ns).
    pub min_step_ns: u64,
    /// Seconds after an outage during which lateness is not learned from.
    pub outage_holdoff_s: u32,
    /// This many outages within `outage_window_s` and they are routine (the
    /// link, not a roam): no hold-off, lateness is learned from.
    pub routine_outages: u32,
    pub outage_window_s: u32,
}

impl Default for AutoConfig {
    fn default() -> Self {
        AutoConfig {
            margin_ns: 1_000_000,
            window_s: 30,
            floor_ns: 3_000_000,
            // A live show's limit; set per show. Above it the link needs a
            // second path, not more latency.
            ceiling_ns: 40_000_000,
            down_rate: 200e-6, // 0.35 cent
            up_rate: 1_000e-6, // 1.7 cent
            min_step_ns: 500_000,
            outage_holdoff_s: 3,
            routine_outages: 3,
            outage_window_s: 60,
        }
    }
}

/// The decision logic, ticked once a second by the receiver.
#[derive(Debug, Clone)]
pub struct AutoLatency {
    pub cfg: AutoConfig,
    quiet_s: u32,
    window_min_ns: Option<i64>,
    /// Highest latency at which a period was late (ns).
    late_at_ns: Option<f64>,
    /// Seconds left in which lateness is ignored (after an outage).
    holdoff_s: u32,
    /// Outages seen, for telemetry.
    pub outages: u64,
    /// Seconds ticked (a clock for the outage window).
    clock_s: u64,
    /// When the most recent outages happened (`clock_s`), newest last.
    recent_outages: [u64; 8],
    /// When it last wanted more than the ceiling (`clock_s`).
    over_ceiling_at: Option<u64>,
    /// Changes made, for telemetry.
    pub lowered: u64,
    pub raised: u64,
}

impl AutoLatency {
    pub fn new(cfg: AutoConfig) -> AutoLatency {
        AutoLatency {
            cfg,
            quiet_s: 0,
            window_min_ns: None,
            late_at_ns: None,
            holdoff_s: 0,
            outages: 0,
            clock_s: 0,
            recent_outages: [u64::MAX; 8],
            over_ceiling_at: None,
            lowered: 0,
            raised: 0,
        }
    }

    /// One second has passed. `now_ns`: the latency in effect; `goal_ns`: the
    /// one being slewed to; `min_headroom_ns`: the smallest headroom this
    /// second (`None`: no period arrived, which counts as none to spare);
    /// `late`: a period arrived too late this second; `outage`: arrivals
    /// stopped altogether this second (see the module docs). Returns a new
    /// goal.
    pub fn tick(
        &mut self,
        now_ns: f64,
        goal_ns: f64,
        min_headroom_ns: Option<i64>,
        late: bool,
        outage: bool,
    ) -> Option<f64> {
        let c = self.cfg;
        self.clock_s += 1;
        if outage {
            self.outages += 1;
            self.recent_outages.rotate_left(1);
            self.recent_outages[7] = self.clock_s;
        }
        let routine = self.routine();
        if outage && !routine {
            self.holdoff_s = c.outage_holdoff_s;
            self.reset();
            return None;
        }
        if self.holdoff_s > 0 && !routine {
            self.holdoff_s -= 1;
            self.reset();
            return None;
        }
        if late || outage {
            self.reset();
            self.late_at_ns = Some(self.late_at_ns.map_or(now_ns, |l| l.max(now_ns)));
            // How late the copy was (negative headroom), plus the margin.
            let by = min_headroom_ns.map_or(0, |h| (-h).max(0)) as f64 + c.margin_ns as f64;
            let want = now_ns.max(goal_ns) + by;
            if want > c.ceiling_ns as f64 {
                self.over_ceiling_at = Some(self.clock_s);
            }
            let g = want.min(c.ceiling_ns as f64);
            if g > goal_ns {
                self.raised += 1;
                return Some(g);
            }
            return None;
        }
        if (now_ns - goal_ns).abs() > 1_000.0 {
            // Still moving: headroom now is not headroom at the goal.
            self.reset();
            return None;
        }
        let h = min_headroom_ns.unwrap_or(0);
        self.window_min_ns = Some(self.window_min_ns.map_or(h, |m| m.min(h)));
        self.quiet_s += 1;
        if self.quiet_s < c.window_s {
            return None;
        }
        let room = self.window_min_ns.unwrap_or(0) - c.margin_ns as i64;
        self.reset();
        if room < c.min_step_ns as i64 {
            return None;
        }
        let step = (room as f64 / 2.0)
            .max(c.min_step_ns as f64)
            .min(room as f64);
        let floor = self.late_at_ns.map_or(c.floor_ns as f64, |l| {
            (l + c.margin_ns as f64).max(c.floor_ns as f64)
        });
        let g = (goal_ns - step).max(floor);
        if g < goal_ns - c.min_step_ns as f64 / 2.0 {
            self.lowered += 1;
            Some(g)
        } else {
            None
        }
    }

    fn reset(&mut self) {
        self.quiet_s = 0;
        self.window_min_ns = None;
    }

    /// Outages are the link's normal behaviour, not roams.
    fn routine(&self) -> bool {
        let n = self.cfg.routine_outages.clamp(1, 8) as usize;
        let window = u64::from(self.cfg.outage_window_s);
        self.recent_outages[8 - n..]
            .iter()
            .all(|&t| t != u64::MAX && self.clock_s - t < window)
    }

    /// In the last minute it needed more than the ceiling: on this link one
    /// path is not enough at this latency (add a second path, move closer).
    pub fn over_ceiling(&self) -> bool {
        self.over_ceiling_at
            .is_some_and(|t| self.clock_s - t < u64::from(self.cfg.outage_window_s))
    }
}

/// Moves `now` towards `goal` by at most `rate` x `dt_ns` (ns of latency per
/// ns of playback).
pub fn slew(now: f64, goal: f64, cfg: &AutoConfig, dt_ns: f64) -> f64 {
    if goal < now {
        (now - cfg.down_rate * dt_ns).max(goal)
    } else {
        (now + cfg.up_rate * dt_ns).min(goal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: f64 = 1e6;

    fn quick() -> AutoConfig {
        AutoConfig {
            window_s: 5,
            ..AutoConfig::default()
        }
    }

    #[test]
    fn lowers_by_the_spare_headroom_after_a_clean_window() {
        let mut a = AutoLatency::new(quick());
        let h = [6.0, 5.2, 7.0, 5.5, 6.1].map(|x| Some((x * MS) as i64));
        let mut out = None;
        for x in h {
            out = a.tick(20.0 * MS, 20.0 * MS, x, false, false);
        }
        // Smallest 5.2 ms, keep 1 ms: 4.2 spare, take half.
        assert_eq!(out, Some(17.9 * MS));
        assert_eq!(a.lowered, 1);
    }

    #[test]
    fn a_late_period_raises_at_once_and_restarts_the_window() {
        let mut a = AutoLatency::new(quick());
        for _ in 0..4 {
            assert_eq!(
                a.tick(10.0 * MS, 10.0 * MS, Some((4.0 * MS) as i64), false, false),
                None
            );
        }
        assert_eq!(
            a.tick(10.0 * MS, 10.0 * MS, Some(0), true, false),
            Some(11.0 * MS)
        );
        assert_eq!(a.raised, 1);
        // The window starts again: four clean seconds are not enough.
        for _ in 0..4 {
            assert_eq!(
                a.tick(11.0 * MS, 11.0 * MS, Some((4.0 * MS) as i64), false, false),
                None
            );
        }
    }

    #[test]
    fn waits_while_slewing_and_respects_floor_ceiling_and_small_gains() {
        let mut a = AutoLatency::new(quick());
        for _ in 0..10 {
            assert_eq!(
                a.tick(12.0 * MS, 10.0 * MS, Some((9.0 * MS) as i64), false, false),
                None
            );
        }
        // Huge headroom: stops at the floor.
        let mut last = None;
        for _ in 0..5 {
            last = a.tick(5.0 * MS, 5.0 * MS, Some((40.0 * MS) as i64), false, false);
        }
        assert_eq!(last, Some(3.0 * MS));
        // Small spare room: at least the minimum step (0.5 ms), never more
        // than the room itself.
        for _ in 0..5 {
            last = a.tick(8.0 * MS, 8.0 * MS, Some((1.6 * MS) as i64), false, false);
        }
        assert_eq!(last, Some(7.5 * MS));
        // Gain under 0.5 ms: leave it.
        for _ in 0..5 {
            last = a.tick(8.0 * MS, 8.0 * MS, Some((1.3 * MS) as i64), false, false);
        }
        assert_eq!(last, None);
        // No periods at all in a second counts as no room.
        for k in 0..5 {
            let h = if k == 2 {
                None
            } else {
                Some((5.0 * MS) as i64)
            };
            last = a.tick(8.0 * MS, 8.0 * MS, h, false, false);
        }
        assert_eq!(last, None);
        // Late at the ceiling: no higher.
        let mut b = AutoLatency::new(AutoConfig {
            ceiling_ns: 20_000_000,
            ..quick()
        });
        assert_eq!(
            b.tick(19.5 * MS, 19.5 * MS, Some(0), true, false),
            Some(20.0 * MS)
        );
        assert_eq!(b.tick(20.0 * MS, 20.0 * MS, Some(0), true, false), None);
    }

    #[test]
    fn never_returns_below_where_it_was_late() {
        let mut a = AutoLatency::new(quick());
        // Late at 11.2 ms: up to 12.2, and 12.2 is the floor from now on.
        assert_eq!(
            a.tick(11.2 * MS, 11.2 * MS, Some(0), true, false),
            Some(12.2 * MS)
        );
        let mut last = None;
        for _ in 0..5 {
            last = a.tick(12.2 * MS, 12.2 * MS, Some((3.0 * MS) as i64), false, false);
        }
        assert_eq!(
            last, None,
            "lots of room in a quiet window, but it was late lower down"
        );
        for _ in 0..5 {
            last = a.tick(14.0 * MS, 14.0 * MS, Some((5.0 * MS) as i64), false, false);
        }
        assert_eq!(
            last,
            Some(12.2 * MS),
            "down to the remembered floor, not below"
        );
    }

    #[test]
    fn an_outage_is_not_learned_as_lateness() {
        let mut a = AutoLatency::new(quick());
        // A roam: arrivals stop, then the queued packets arrive late.
        assert_eq!(a.tick(10.0 * MS, 10.0 * MS, None, true, true), None);
        for _ in 0..3 {
            assert_eq!(a.tick(10.0 * MS, 10.0 * MS, Some(0), true, false), None);
        }
        assert_eq!((a.raised, a.outages), (0, 1));
        // After the hold-off, lateness counts again (and is remembered).
        assert_eq!(
            a.tick(10.0 * MS, 10.0 * MS, Some(0), true, false),
            Some(11.0 * MS)
        );
        // And a window must be clean from scratch before any lowering.
        let mut b = AutoLatency::new(quick());
        for _ in 0..4 {
            b.tick(20.0 * MS, 20.0 * MS, Some((9.0 * MS) as i64), false, false);
        }
        b.tick(20.0 * MS, 20.0 * MS, None, false, true);
        let mut last = None;
        for _ in 0..(3 + 4) {
            last = b.tick(20.0 * MS, 20.0 * MS, Some((9.0 * MS) as i64), false, false);
        }
        assert_eq!(
            last, None,
            "hold-off plus four clean seconds is not a window"
        );
        last = b.tick(20.0 * MS, 20.0 * MS, Some((9.0 * MS) as i64), false, false);
        assert!(last.is_some());
    }

    #[test]
    fn raises_by_how_late_it_was_up_to_the_ceiling_and_says_so() {
        let mut a = AutoLatency::new(quick());
        // 6 ms late: up by 6 ms plus the 1 ms margin, at once.
        assert_eq!(
            a.tick(20.0 * MS, 20.0 * MS, Some((-6.0 * MS) as i64), true, false),
            Some(27.0 * MS)
        );
        assert!(!a.over_ceiling());
        // 30 ms late at 27 ms: the ceiling (40 ms) stops it, and says so.
        assert_eq!(
            a.tick(27.0 * MS, 27.0 * MS, Some((-30.0 * MS) as i64), true, false),
            Some(40.0 * MS)
        );
        assert!(a.over_ceiling(), "the link needs more than the ceiling");
        // The warning clears a minute after the last time it was needed.
        for _ in 0..60 {
            a.tick(40.0 * MS, 40.0 * MS, Some((5.0 * MS) as i64), false, false);
        }
        assert!(!a.over_ceiling());
    }

    #[test]
    fn routine_outages_are_learned_from_a_rare_one_is_excused() {
        let mut a = AutoLatency::new(quick());
        // One outage (a roam): excused, as before.
        assert_eq!(
            a.tick(20.0 * MS, 20.0 * MS, Some(-15 * MS as i64), true, true),
            None
        );
        for _ in 0..10 {
            a.tick(20.0 * MS, 20.0 * MS, Some((5.0 * MS) as i64), false, false);
        }
        // A second within the minute: still excused.
        assert_eq!(
            a.tick(20.0 * MS, 20.0 * MS, Some(-15 * MS as i64), true, true),
            None
        );
        for _ in 0..10 {
            a.tick(20.0 * MS, 20.0 * MS, Some((5.0 * MS) as i64), false, false);
        }
        // The third within a minute: this link stalls routinely, so it is
        // lateness: raise by how late (15 ms) plus the margin.
        assert_eq!(
            a.tick(20.0 * MS, 20.0 * MS, Some(-15 * MS as i64), true, true),
            Some(36.0 * MS)
        );
        // Three outages spread over more than a minute are still roams.
        let mut b = AutoLatency::new(quick());
        for _ in 0..3 {
            assert_eq!(b.tick(20.0 * MS, 20.0 * MS, Some(0), true, true), None);
            for _ in 0..40 {
                b.tick(20.0 * MS, 20.0 * MS, Some((0.5 * MS) as i64), false, false);
            }
        }
        assert_eq!(b.raised, 0);
    }

    #[test]
    fn slew_rates() {
        let c = AutoConfig::default();
        // One second of playback: down 0.2 ms, up 1 ms, never past the goal.
        assert!((slew(20.0 * MS, 10.0 * MS, &c, 1e9) - 19.8 * MS).abs() < 1.0);
        assert!((slew(10.0 * MS, 20.0 * MS, &c, 1e9) - 11.0 * MS).abs() < 1.0);
        assert_eq!(slew(10.0 * MS, 10.1 * MS, &c, 1e9), 10.1 * MS);
    }
}
