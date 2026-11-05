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

/// A task's class. Audio is the one that must not be starved: it is the class AirMatrix's engine runs in, and a
/// priority is only honest if a lower class cannot take its CPU while it is ready.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Priority {
    /// The audio path. Highest.
    Audio = 0,
    /// Everything else.
    Normal = 1,
}

/// A task's saved frame, in the layout the interrupt path saves: 31 registers at 0..240, `x30` at 240,
/// `ELR_EL1` at 248, `SPSR_EL1` at 256, 288 bytes total (16-byte aligned).
pub const FRAME_WORDS: usize = 288 / 8;
pub const ELR_SLOT: usize = 248 / 8;
pub const SPSR_SLOT: usize = 256 / 8;

/// The PSTATE a task starts in: EL1h with interrupts enabled. Getting this wrong is how a first switch dies — an
/// SPSR of zero puts the CPU in EL0 with interrupts masked, and nothing afterwards makes sense.
pub const SPSR_EL1H_UNMASKED: u64 = 0b00101;

/// Build a frame for a task that has never run: every register zero, `x30` unused, `ELR` pointed at the entry and
/// `SPSR` set to "EL1h, interrupts on". Returns the stack pointer the switch must restore from — the frame itself.
///
/// This exists separately from the switch so that the frame can be inspected before anything jumps to it: a frame
/// that is wrong is a hang with no output, and a hang with no output is the most expensive failure this project has.
pub fn install_frame(stack_top: usize, entry: usize) -> usize {
    let frame = (stack_top - FRAME_WORDS * 8) & !0xf;
    // SAFETY: the caller guarantees `stack_top` is the top of a writable stack at least 288 bytes deep, and nothing
    // else is using it.
    unsafe {
        let words = frame as *mut u64;
        for i in 0..FRAME_WORDS {
            core::ptr::write_volatile(words.add(i), 0);
        }
        core::ptr::write_volatile(words.add(ELR_SLOT), entry as u64);
        core::ptr::write_volatile(words.add(SPSR_SLOT), SPSR_EL1H_UNMASKED);
    }
    frame
}

/// What `install_frame` put in the two slots that make a frame resumable. Read back by the boot so the frame is
/// checked before anything jumps to it.
pub fn frame_state(frame: usize) -> (u64, u64) {
    // SAFETY: the frame was built by `install_frame` and is inside a task stack.
    unsafe {
        let words = frame as *const u64;
        (
            core::ptr::read_volatile(words.add(ELR_SLOT)),
            core::ptr::read_volatile(words.add(SPSR_SLOT)),
        )
    }
}

/// The second task's stack. A static, because a task's stack must exist before its frame can point into it.
#[repr(align(16))]
pub struct TaskStack(pub [u8; 16 * 1024]);
pub static mut TASK_B_STACK: TaskStack = TaskStack([0; 16 * 1024]);

/// Where each task's saved frame lives. A frame is a stack pointer: the scheduler stores the outgoing one and returns
/// the incoming one, which is the whole of a switch on this machine.
pub static mut CTX: [usize; 2] = [0; 2];

/// Which task is running. Only meaningful once switching is enabled.
pub static mut CURRENT: usize = 0;

/// Switching is ordered by the boot, not by the interrupt, so that it cannot start before the second task's frame
/// exists — a switch to a zero frame is a jump to address zero, which is a hang with no output.
pub static mut SWITCHING: bool = false;

pub fn enable_switching() {
    // SAFETY: ordered by the boot on one core with interrupts masked.
    unsafe { SWITCHING = true };
}

pub fn switching() -> bool {
    // SAFETY: read-only here.
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(SWITCHING)) }
}

/// The interrupt path's decision: record the frame that was interrupted, and return the frame to resume from.
///
/// With switching off it returns `sp` unchanged — identical to before this existed, which is how it is measured. With
/// switching on and the other task's frame present, it records and returns the other task's: that is a context
/// switch, in two stores and a load.
pub fn on_irq(sp: usize) -> usize {
    if !switching() {
        return sp;
    }
    // SAFETY: single core, interrupts masked in the handler, and only the handler and the boot touch these.
    unsafe {
        let cur = core::ptr::read_volatile(core::ptr::addr_of!(CURRENT));
        let ctx = core::ptr::addr_of_mut!(CTX) as *mut usize;
        core::ptr::write_volatile(ctx.add(cur), sp);

        let next = (cur + 1) % 2;
        let incoming = core::ptr::read_volatile(ctx.add(next));
        if incoming == 0 {
            // The other task has no frame yet. Running the same task again is the honest answer; switching to zero
            // would be a jump into nothing.
            return sp;
        }
        // The mutation: record the intent but answer with the same frame, so the machine looks alive and never runs
        // anything else. Every counter still moves; only the sequence is wrong.
        if cfg!(feature = "sched-mutant-no-switch") {
            return sp;
        }
        static mut SW: u64 = 0;
        SW += 1;
        if SW % 100 == 0 {
            let mut l = crate::uart::LineBuf::new();
            l.push_str("SWITCH ");
            l.push_u64(SW);
            l.push_str(": cur ");
            l.push_u64(cur as u64);
            l.push_str(" -> next ");
            l.push_u64(next as u64);
            l.push_str(", incoming ");
            l.push_u64(incoming);
            l.flush(&crate::uart::Uart::at(crate::uart::UART_QEMU_VIRT));
        }
        core::ptr::write_volatile(core::ptr::addr_of_mut!(CURRENT), next);
        core::ptr::write_volatile(core::ptr::addr_of_mut!(MOVED), core::ptr::read_volatile(core::ptr::addr_of!(MOVED)) + 1);
        incoming
    }
}

/// Install the first task's own frame, so that the very first switch has somewhere to come back to.
pub fn record_current(sp: usize) {
    // SAFETY: ordered by the boot before switching is enabled.
    unsafe {
        let ctx = core::ptr::addr_of_mut!(CTX) as *mut usize;
        core::ptr::write_volatile(ctx, sp);
    }
}

/// Install the second task's frame, so the first switch has somewhere to go.
pub fn install_second(frame: usize) {
    // SAFETY: ordered by the boot before switching is enabled.
    unsafe {
        let ctx = core::ptr::addr_of_mut!(CTX) as *mut usize;
        core::ptr::write_volatile(ctx.add(1), frame);
    }
}

/// How many times the CPU actually moved between tasks. Read by the boot so the count is evidence, not belief.
pub static mut MOVED: u64 = 0;

pub fn moved() -> u64 {
    // SAFETY: written only by the interrupt handler.
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(MOVED)) }
}

/// The tasks this rung knows about. Two is enough to prove a choice is being made.
pub const TASKS: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TaskId(pub usize);

/// Ready flags and the current choice. A task is "ready" if it can run; on this rung both start ready and stay ready.
pub struct Scheduler {
    ready: [bool; TASKS],
    priority: [Priority; TASKS],
    current: TaskId,
    switches: u64,
}

impl Scheduler {
    pub const fn new() -> Self {
        Self {
            ready: [false; TASKS],
            priority: [Priority::Normal; TASKS],
            current: TaskId(0),
            switches: 0,
        }
    }

    pub fn admit(&mut self, task: TaskId) {
        self.admit_with_priority(task, Priority::Normal);
    }

    pub fn admit_with_priority(&mut self, task: TaskId, priority: Priority) {
        if task.0 < TASKS {
            self.ready[task.0] = true;
            self.priority[task.0] = priority;
        }
    }

    pub fn priority_of(&self, task: TaskId) -> Priority {
        self.priority[task.0]
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
        // Priority first: the best class among the ready tasks. Then round-robin among the tasks in that class, so a
        // class with two tasks does not starve one of them.
        if cfg!(feature = "sched-mutant-ignore-priority") {
            for step in 1..=TASKS {
                let candidate = (self.current.0 + step) % TASKS;
                if self.ready[candidate] {
                    self.switches += 1;
                    self.current = TaskId(candidate);
                    return Some(self.current);
                }
            }
            return None;
        }
        let mut best: Option<Priority> = None;
        for task in 0..TASKS {
            if self.ready[task] {
                let p = self.priority[task];
                best = Some(match best {
                    Some(b) if b <= p => b,
                    _ => p,
                });
            }
        }
        let best = best?;
        for step in 1..=TASKS {
            let candidate = (self.current.0 + step) % TASKS;
            if self.ready[candidate] && self.priority[candidate] == best {
                self.switches += 1;
                self.current = TaskId(candidate);
                return Some(self.current);
            }
        }
        None
    }
}
