//! Roaming between access points without a dropout: a node with two radios
//! lets only one of them leave its access point at a time.
//!
//! A radio that roams (or scans for a better access point, which takes it off
//! its channel) stops carrying packets for tens to hundreds of milliseconds.
//! With every packet sent over both radios, that is harmless as long as the
//! *other* radio is carrying them meanwhile. Left alone, operating systems
//! roam and scan each radio on its own schedule, and both often fade at the
//! same moment (the presenter walked away from both access points). This
//! coordinator takes those decisions instead, once a second:
//!
//! * a radio may scan or roam only while every other radio has been healthy
//!   (associated, decent signal, its path answering) for `settle_s`;
//! * roam when the current access point is weak and a clearly better one has
//!   been seen (hysteresis), never more often than `min_dwell_s`;
//! * scan the weakest radio first, one radio at a time, and only when a scan
//!   is due or its signal is falling;
//! * stationary nodes (stage boxes, front of house) are locked: never roam.
//!
//! A single-radio node cannot be covered: it roams only when its signal is
//! poor anyway (the gap is then shorter than the dropout it avoids), and the
//! receiver recognises the gap as an outage. Pure logic; the platform part
//! (reading links, telling the supplicant) lives in `rob-io`.

/// What the platform reports about one radio, once a second.
use crate::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Radio {
    /// Associated to an access point.
    pub up: bool,
    /// Current access point and its signal.
    pub bssid: Option<[u8; 6]>,
    pub signal_dbm: Option<i32>,
    /// Best other access point of the same network heard in the last scan.
    pub candidate: Option<([u8; 6], i32)>,
    /// Its path is delivering (e.g. sync answers came back over it this
    /// second).
    pub delivering: bool,
}

impl Radio {
    fn healthy(&self, cfg: &RoamConfig) -> bool {
        self.up && self.delivering && self.signal_dbm.is_some_and(|s| s >= cfg.healthy_dbm)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoamConfig {
    /// Never roam (stationary node).
    pub locked: bool,
    /// A radio is healthy at or above this signal (and delivering).
    pub healthy_dbm: i32,
    /// Consider roaming below this signal.
    pub roam_below_dbm: i32,
    /// The candidate must be at least this much stronger.
    pub better_by_db: i32,
    /// The other radios must have been healthy this long.
    pub settle_s: f64,
    /// Least time between two roams of one radio.
    pub min_dwell_s: f64,
    /// Scan a radio this often when it is weak (it costs that radio a gap).
    pub scan_every_s: f64,
    /// A roam or scan counts as finished once its radio is healthy again
    /// and at least this long has passed (a scan may never show as a gap at
    /// one-second resolution).
    pub action_min_s: f64,
    /// A roam or scan that has not finished after this long is given up on.
    pub action_timeout_s: f64,
    /// Serialise power save (design 24 section 4.2): at most one radio may
    /// sleep, and never while the other acts. Off by default — mains-powered
    /// show nodes keep power save off entirely (section 4.1).
    pub power_save: bool,
}

impl Default for RoamConfig {
    fn default() -> Self {
        RoamConfig {
            locked: false,
            healthy_dbm: -75,
            roam_below_dbm: -68,
            better_by_db: 8,
            settle_s: 3.0,
            min_dwell_s: 10.0,
            scan_every_s: 20.0,
            action_min_s: 2.0,
            action_timeout_s: 5.0,
            power_save: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Hold,
    /// Look for better access points on this radio.
    Scan {
        radio: usize,
    },
    /// Move this radio to that access point.
    Roam {
        radio: usize,
        to: [u8; 6],
    },
    /// Turn this radio's power save off (wake it) or on (let it sleep).
    /// Serialised like roams and scans: never two radios sleeping at once.
    PowerSave {
        radio: usize,
        on: bool,
    },
}

#[derive(Debug, Clone)]
pub struct Coordinator {
    pub cfg: RoamConfig,
    /// Per radio: healthy since (s), last roam (s), last scan (s).
    healthy_since: Vec<Option<f64>>,
    last_roam: Vec<Option<f64>>,
    last_scan: Vec<Option<f64>>,
    /// The action in progress: (radio, started).
    busy: Option<(usize, f64)>,
    /// Per radio: power save is on (the radio may sleep).
    sleeping: Vec<bool>,
    pub roams: u64,
    pub scans: u64,
    /// Times a radio wanted to move but another was not settled.
    pub deferred: u64,
}

impl Coordinator {
    pub fn new(cfg: RoamConfig, radios: usize) -> Coordinator {
        Coordinator {
            cfg,
            healthy_since: vec![None; radios],
            last_roam: vec![None; radios],
            last_scan: vec![None; radios],
            busy: None,
            sleeping: vec![false; radios],
            roams: 0,
            scans: 0,
            deferred: 0,
        }
    }

    /// Decides what to do now (`now_s`: any monotonic seconds).
    pub fn tick(&mut self, now_s: f64, radios: &[Radio]) -> Action {
        let c = self.cfg;
        for (k, r) in radios.iter().enumerate().take(self.healthy_since.len()) {
            if r.healthy(&c) {
                self.healthy_since[k].get_or_insert(now_s);
            } else {
                self.healthy_since[k] = None;
            }
        }
        if let Some((k, started)) = self.busy {
            // Busy until that radio is healthy again (or it timed out).
            let back = self.healthy_since[k].is_some() && now_s - started >= c.action_min_s;
            if !back && now_s - started < c.action_timeout_s {
                return Action::Hold;
            }
            self.busy = None;
        }
        if c.locked {
            return Action::Hold;
        }
        let n = radios.len().min(self.healthy_since.len());
        // Settled: healthy, and for `settle_s` since it became healthy and
        // since its own last scan or roam.
        let settled = |j: usize, me: &Coordinator| {
            // A sleeping radio is resting, not a problem: it does not block
            // the others (its own wake is decided by its signal below).
            if me.sleeping[j] {
                return true;
            }
            me.healthy_since[j].is_some_and(|t| {
                let acted = me.last_roam[j]
                    .into_iter()
                    .chain(me.last_scan[j])
                    .fold(t, f64::max);
                now_s - acted >= c.settle_s
            })
        };
        // Weakest radio first.
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&k| radios[k].signal_dbm.unwrap_or(i32::MIN));
        let weakest = order.first().copied();
        for k in order {
            let r = &radios[k];
            let weak = !r.up || r.signal_dbm.map_or(true, |s| s < c.roam_below_dbm);
            if c.power_save && weak && self.sleeping[k] {
                // It needs to look around but is asleep: wake it first.
                self.sleeping[k] = false;
                return Action::PowerSave {
                    radio: k,
                    on: false,
                };
            }
            if !weak {
                continue;
            }
            let dwell_ok = self.last_roam[k].map_or(true, |t| now_s - t >= c.min_dwell_s);
            let better = r.candidate.filter(|&(b, s)| {
                Some(b) != r.bssid && r.signal_dbm.map_or(true, |cur| s >= cur + c.better_by_db)
            });
            let want = match better {
                Some((to, _)) if dwell_ok => Some(Action::Roam { radio: k, to }),
                _ if self.last_scan[k].map_or(true, |t| now_s - t >= c.scan_every_s) => {
                    Some(Action::Scan { radio: k })
                }
                _ => None,
            };
            let Some(action) = want else { continue };
            // A single radio has nobody to cover it: it may act only when it
            // is really poor (or down) anyway.
            let alone = n == 1;
            let allowed = if alone {
                !r.healthy(&c) || r.signal_dbm.is_some_and(|s| s < c.healthy_dbm + 3)
            } else {
                (0..n).filter(|&j| j != k).all(|j| settled(j, self))
            };
            if !allowed {
                self.deferred += 1;
                continue;
            }
            match action {
                Action::Roam { .. } => {
                    self.roams += 1;
                    self.last_roam[k] = Some(now_s);
                }
                Action::Scan { .. } => {
                    self.scans += 1;
                    self.last_scan[k] = Some(now_s);
                }
                Action::Hold | Action::PowerSave { .. } => {}
            }
            self.busy = Some((k, now_s));
            return action;
        }
        // Nothing to move or scan: let the weakest healthy radio sleep, one at
        // a time, so the two radios never sleep together (design 24 section
        // 4.2).
        if c.power_save {
            if let Some(k) = weakest {
                if radios[k].healthy(&c) && !self.sleeping.iter().any(|&s| s) {
                    self.sleeping[k] = true;
                    return Action::PowerSave { radio: k, on: true };
                }
            }
        }
        Action::Hold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [u8; 6] = [1, 0, 0, 0, 0, 1];
    const B: [u8; 6] = [1, 0, 0, 0, 0, 2];

    fn radio(bssid: [u8; 6], dbm: i32, cand: Option<([u8; 6], i32)>) -> Radio {
        Radio {
            up: true,
            bssid: Some(bssid),
            signal_dbm: Some(dbm),
            candidate: cand,
            delivering: true,
        }
    }

    #[test]
    fn strong_radios_are_left_alone() {
        let mut c = Coordinator::new(RoamConfig::default(), 2);
        for t in 0..60 {
            let r = [radio(A, -50, Some((B, -40))), radio(B, -55, None)];
            assert_eq!(c.tick(f64::from(t), &r), Action::Hold);
        }
    }

    #[test]
    fn one_radio_at_a_time_and_only_once_the_other_is_settled() {
        let mut c = Coordinator::new(RoamConfig::default(), 2);
        // Both weak, both with a better access point in sight (the presenter
        // walked away from A towards B).
        let both_weak = [radio(A, -72, Some((B, -55))), radio(A, -70, Some((B, -56)))];
        // Not settled yet (healthy for under 3 s): wait.
        assert_eq!(c.tick(0.0, &both_weak), Action::Hold);
        assert_eq!(c.tick(1.0, &both_weak), Action::Hold);
        assert_eq!(c.tick(2.0, &both_weak), Action::Hold);
        // Settled: the weakest (radio 0) moves first.
        assert_eq!(c.tick(3.0, &both_weak), Action::Roam { radio: 0, to: B });
        // While radio 0 is away (down), radio 1 must not move.
        let away = [
            Radio {
                up: false,
                ..both_weak[0]
            },
            both_weak[1],
        ];
        for t in 4..7 {
            assert_eq!(c.tick(f64::from(t), &away), Action::Hold);
        }
        // Radio 0 is back on B and healthy; radio 1 waits until 0 has been
        // healthy for 3 s, then follows.
        let back = [radio(B, -52, None), both_weak[1]];
        assert_eq!(
            c.tick(7.0, &back),
            Action::Hold,
            "busy until radio 0 is back"
        );
        assert_eq!(c.tick(8.0, &back), Action::Hold);
        assert_eq!(c.tick(9.0, &back), Action::Hold);
        assert_eq!(c.tick(10.0, &back), Action::Roam { radio: 1, to: B });
        assert_eq!(c.roams, 2);
        assert!(c.deferred > 0);
    }

    #[test]
    fn hysteresis_dwell_and_scans() {
        let cfg = RoamConfig {
            settle_s: 0.0,
            ..RoamConfig::default()
        };
        let mut c = Coordinator::new(cfg, 2);
        let good = radio(B, -50, None);
        // Weak, but the candidate is only 5 dB better: scan, do not roam.
        let r = [radio(A, -70, Some((B, -65))), good];
        assert_eq!(c.tick(0.0, &r), Action::Scan { radio: 0 });
        // Busy until the scan is over (radio healthy again), then no new
        // scan for 20 s.
        let r_ok = [
            Radio {
                delivering: true,
                ..r[0]
            },
            good,
        ];
        assert_eq!(c.tick(1.0, &r_ok), Action::Hold);
        for t in 2..20 {
            assert_eq!(c.tick(f64::from(t), &r_ok), Action::Hold, "t={t}");
        }
        assert_eq!(c.tick(20.0, &r_ok), Action::Scan { radio: 0 });
        // A clearly better one: roam; and not again within 10 s.
        let r2 = [radio(A, -71, Some((B, -55))), good];
        assert_eq!(c.tick(21.0, &r2), Action::Hold, "previous scan still busy");
        let mut c = Coordinator::new(cfg, 2);
        assert_eq!(c.tick(0.0, &r2), Action::Roam { radio: 0, to: B });
        let r3 = [radio(B, -71, Some((A, -50))), good];
        assert_eq!(c.tick(1.0, &r3), Action::Hold);
        assert!(matches!(
            c.tick(2.0, &r3),
            Action::Scan { .. } | Action::Hold
        ));
    }

    #[test]
    fn locked_nodes_never_move_and_lone_radios_only_when_poor() {
        let mut locked = Coordinator::new(
            RoamConfig {
                locked: true,
                ..RoamConfig::default()
            },
            2,
        );
        let r = [radio(A, -80, Some((B, -50))), radio(A, -80, Some((B, -50)))];
        for t in 0..30 {
            assert_eq!(locked.tick(f64::from(t), &r), Action::Hold);
        }
        let mut lone = Coordinator::new(RoamConfig::default(), 1);
        // -70: weak by the roam threshold, but still healthy: hold on.
        assert_eq!(
            lone.tick(0.0, &[radio(A, -70, Some((B, -50)))]),
            Action::Hold
        );
        // -74: nearly unhealthy: the gap is worth it now.
        assert_eq!(
            lone.tick(1.0, &[radio(A, -74, Some((B, -50)))]),
            Action::Roam { radio: 0, to: B }
        );
    }

    #[test]
    fn a_stuck_action_times_out() {
        let cfg = RoamConfig {
            settle_s: 0.0,
            ..RoamConfig::default()
        };
        let mut c = Coordinator::new(cfg, 2);
        let r = [radio(A, -72, Some((B, -50))), radio(B, -50, None)];
        assert_eq!(c.tick(0.0, &r), Action::Roam { radio: 0, to: B });
        let stuck = [Radio { up: false, ..r[0] }, r[1]];
        for t in 1..5 {
            assert_eq!(c.tick(f64::from(t), &stuck), Action::Hold);
        }
        // Timed out at 5 s; radio 0 is down and weak, but its dwell (10 s)
        // forbids another roam, so it scans instead.
        assert_eq!(c.tick(5.0, &stuck), Action::Scan { radio: 0 });
    }

    #[test]
    fn power_save_is_serialised_one_radio_at_a_time() {
        let mut c = Coordinator::new(
            RoamConfig {
                power_save: true,
                ..RoamConfig::default()
            },
            2,
        );
        // Both healthy; the weaker (radio 1) sleeps first.
        let both = [radio(A, -50, None), radio(B, -55, None)];
        assert_eq!(c.tick(0.0, &both), Action::PowerSave { radio: 1, on: true });
        // One is sleeping: the other does not sleep, and nothing else moves.
        for t in 1..5 {
            assert_eq!(c.tick(f64::from(t), &both), Action::Hold, "t={t}");
        }
        // The sleeping radio's signal drops and a better access point is in
        // sight: it wakes, then roams on the next tick.
        let weak = [radio(A, -50, None), radio(B, -72, Some((A, -55)))];
        assert_eq!(
            c.tick(5.0, &weak),
            Action::PowerSave {
                radio: 1,
                on: false
            }
        );
        assert_eq!(c.tick(6.0, &weak), Action::Roam { radio: 1, to: A });
    }
}
