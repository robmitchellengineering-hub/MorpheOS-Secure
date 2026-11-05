//! The memory the machine owns — the allocator, before the MMU.
//!
//! WHY THIS EXISTS BEFORE ANYTHING ELSE. Every later rung needs memory it can hand out: page tables (M0b), task
//! stacks (M1b), packet buffers (the network), a show's scene list. Rust's `alloc` crate gives the interface, and this
//! file gives the machine's own implementation of it — a first-fit free list over a static arena, with a header that
//! makes a double free **detectable** rather than silently corrupting the list.
//!
//! WHAT IT IS NOT. It is not a page allocator, not a slab, not power-of-two in speed, and it does not return memory to
//! the MMU. It is a correct allocator for the Core's own small objects on rungs M0–M2, and it says so.
//!
//! THE BUG IT IS BUILT TO CATCH. A double free is the classic way a free-list allocator hands the same bytes to two
//! owners: the second `free` inserts the block twice, and the next two allocations overlap. Every block carries a
//! `USED` magic that is cleared on free and re-checked on free, so a double free is refused and named instead of
//! quietly creating aliasing. `scripts/m2b.sh` attacks it, and its `--self-test` turns the check off and requires the
//! attack to succeed — a guard that has never failed is a guard nobody has tested.

use core::alloc::{GlobalAlloc, Layout};
use core::mem::size_of;
use core::ptr;

/// The arena. 256 KiB is enough for the Core's own objects on this rung and small enough to be a decision rather than
/// a default: growing it is a deliberate change, because memory the Core hands out is memory the Core owns.
const ARENA_SIZE: usize = 640 * 1024;

#[repr(align(128))]
struct Arena([u8; ARENA_SIZE]);

static mut ARENA: Arena = Arena([0; ARENA_SIZE]);

/// The header every allocation carries. `magic` is what makes a double free detectable; `size` is what makes `free`
/// possible without the caller repeating the layout. The header is padded to 128 bytes so the payload that follows it
/// is 128-aligned (rtrb's CachePadded — inside the engine's Arc<RingBuffer> — is 128-aligned on aarch64).
#[repr(C)]
struct Header {
    magic: u64,
    size: usize,
    next: *mut Header,
    _pad: [u8; 104],
}

const USED: u64 = 0x4d4f5250_48454f53; // "MORPHEOS" — a magic that is not zero, so zeroed memory is not "used".
const FREE: u64 = 0x4d4f5250_48524545; // "MORPHE" + "FRE" — distinct from USED by construction.

const HEADER: usize = size_of::<Header>();

pub struct FirstFit {
    inner: core::cell::UnsafeCell<State>,
}

/// The Core runs on one core with interrupts off on this rung, so the allocator is not re-entered. When multiprocessor
/// bring-up lands (M0b) this becomes a spin lock — stated here rather than discovered.
unsafe impl Sync for FirstFit {}

struct State {
    head: *mut Header,
    used: usize,
    frees: usize,
    double_frees_refused: usize,
}

impl FirstFit {
    pub const fn new() -> Self {
        Self { inner: core::cell::UnsafeCell::new(State { head: ptr::null_mut(), used: 0, frees: 0, double_frees_refused: 0 }) }
    }

    fn state(&self) -> *mut State {
        self.inner.get()
    }

    /// Hand the arena to the free list. Called once from `kmain` before anything allocates.
    ///
    /// # Safety
    /// Must be called exactly once, before any allocation, and `ARENA` must not be used directly elsewhere.
    pub unsafe fn init(&self) {
        let st = self.state();
        let base = core::ptr::addr_of_mut!(ARENA.0).cast::<u8>();
        let header = base.cast::<Header>();
        // SAFETY: the arena is ARENA_SIZE bytes and 16-byte aligned, so a Header fits at its start, and the block that
        // follows it is `ARENA_SIZE - HEADER` bytes.
        unsafe {
            ptr::write(header, Header { magic: FREE, size: ARENA_SIZE - HEADER, next: ptr::null_mut() });
        }
        unsafe {
            (*st).head = header;
            (*st).used = 0;
            (*st).frees = 0;
            (*st).double_frees_refused = 0;
        }
    }

    /// First fit: walk the list, take the first block that is big enough, split it if the remainder can hold a header
    /// plus a useful allocation, and return the payload pointer.
    pub unsafe fn alloc_block(&self, size: usize) -> *mut u8 {
        let st = self.state();
        // 128-byte alignment: rtrb's CachePadded (used inside the engine's
        // Arc<RingBuffer>) is 128-aligned on aarch64, so every allocation must
        // satisfy that ceiling.
        let want = align_up(size.max(1), 128);
        let mut cursor = unsafe { (*st).head };
        let mut prev: *mut Header = ptr::null_mut();
        while !cursor.is_null() {
            // SAFETY: every node in this list was written by `init` or by this function, and is inside the arena.
            let node = unsafe { &mut *cursor };
            // A free block already owns its header, so a block of exactly `want` satisfies the request. Requiring
            // `want + HEADER` here refused to reuse an exactly-sized hole - measured: the freed 32-byte cue list was
            // skipped by the next 32-byte allocation.
            if node.magic == FREE && node.size >= want {
                let remainder = node.size - want - HEADER;
                if remainder >= 128 {
                    // Split: the tail becomes a new free block.
                    // SAFETY: `remainder` bytes were verified to be inside this block.
                    let split = unsafe { (cursor.cast::<u8>().add(HEADER + want)).cast::<Header>() };
                    unsafe {
                        ptr::write(split, Header { magic: FREE, size: remainder, next: node.next });
                    }
                    node.size = want;
                    node.next = split;
                } else if remainder > 0 {
                    // Too small to split: the allocation swallows the remainder, and its recorded size says so.
                    node.size = want + remainder;
                }
                unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!((*cursor).magic), USED) };
                unsafe { (*st).used += 1 };
                // SAFETY: the payload starts right after the header of a block this function owns.
                return unsafe { cursor.cast::<u8>().add(HEADER) };
            }
            prev = cursor;
            cursor = node.next;
        }
        let _ = prev;
        ptr::null_mut()
    }

    /// Return a block. A block whose magic is not USED is a double free (or a pointer that is not ours), and it is
    /// refused: the block is NOT inserted into the list again, so no two owners can ever be handed the same bytes.
    pub unsafe fn free_block(&self, payload: *mut u8) -> bool {
        let st = self.state();
        if payload.is_null() {
            return false;
        }
        // SAFETY: the caller promises the pointer came from this allocator; the header is one HEADER before it.
        let header = unsafe { payload.sub(HEADER).cast::<Header>() };
        let magic = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*header).magic)) };
        let node = unsafe { &mut *header };
        if magic != USED {
            // The mutation: a build that trusts the caller turns this refusal off, and the falsifier's self-test
            // requires the double free to then be ACCEPTED (no refusal line).
            if cfg!(feature = "m2b-mutant-allow-double-free") {
                return true;
            }
            unsafe { (*st).double_frees_refused += 1 };
            return false;
        }
        unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!((*header).magic), FREE) };
        unsafe { (*st).frees += 1 };
        true
    }

    /// The magic of the block owning `payload`, read the same way `free_block` reads it. Exists so a test can show
    /// three numbers - address and flag at allocate, at the first free, and at the second - instead of guessing.
    pub unsafe fn magic_of(&self, payload: *mut u8) -> u64 {
        let header = unsafe { payload.sub(HEADER).cast::<Header>() };
        unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*header).magic)) }
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        let st = self.state();
        unsafe { ((*st).used, (*st).frees, (*st).double_frees_refused) }
    }
}

fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

unsafe impl GlobalAlloc for FirstFit {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the GlobalAlloc contract gives this call exclusive access to the allocator's state.
        unsafe { self.alloc_block(layout.size()) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        // SAFETY: as above; `free_block` refuses a pointer whose header is not USED rather than trusting the caller.
        unsafe {
            self.free_block(ptr);
        }
    }
}
