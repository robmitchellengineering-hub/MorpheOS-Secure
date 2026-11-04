//! The scheduler's POLICY — rung M5c, first half: choosing which task runs next, with no assembly.
//!
//! WHY THIS IS SPLIT FROM THE SWITCH. Preemption has two halves that fail differently: the **policy** (who runs next)
//! and the **mechanism** (how the CPU is moved). Writing them together means a bug in either looks like the other, and
//! this session has already paid for that lesson on the interrupt rung. The policy is pure logic and can be measured in
//! a boot without a single register saved; the mechanism comes next and is the only risky part left.
//!
//! WHAT IT IS: two ready tasks, round-robin, one quantum each. That is the smallest policy that is not "always the same
//! task" — and "always the same task" is the failure a scheduler is most likely to have, because it looks exactly
//! like a working scheduler from the outside.
//!
//! WHAT IT IS NOT: no priorities (M5d), no deadlines, no blocking, no more than two tasks. A real-time class needs a
//! budget, and a budget needs the certificate (M7a) to be more than an assertion.

/// The tasks this rung knows about. Two is enough to prove a choice is being made.
pub const TASKS: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TaskId(pub usize);

/// Ready flags and the current choice. A task is "ready" if it can run; on this rung both start ready and stay ready.
pub struct Scheduler {
    ready: [bool; TASKS],
    current: TaskId,
    switches: u64,
}

impl Scheduler {
    pub const fn new() -> Self {
        Self { ready: [false; TASKS], current: TaskId(0), switches: 0 }
    }

    pub fn admit(&mut self, task: TaskId) {
        if task.0 < TASKS {
            self.ready[task.0] = true;
        }
    }

    pub fn current(&self) -> TaskId {
        self.current
    }

    pub fn switches(&self) -> u64 {
        self.switches
    }

    /// Pick the next ready task, round-robin from the current one. Returns `None` if nothing is ready — a machine with
    /// no runnable task must be able to say so rather than pick one anyway.
    pub fn pick_next(&mut self) -> Option<TaskId> {
        // The mutation: always the same task. It returns a plausible answer, keeps the counter moving, and never runs
        // anything else — which is why the falsifier watches the *sequence*, not the count.
        if cfg!(feature = "sched-mutant-always-first") {
            self.switches += 1;
            return Some(TaskId(0));
        }
        for step in 1..=TASKS {
            let candidate = (self.current.0 + step) % TASKS;
            if self.ready[candidate] {
                self.switches += 1;
                self.current = TaskId(candidate);
                return Some(self.current);
            }
        }
        None
    }
}
