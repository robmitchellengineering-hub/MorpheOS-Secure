#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use core::panic::PanicInfo;

use rob_core::rtp::{
    self, RobCore, RtpHeader, DEFAULT_ROB_EXT_ID, MODE_BIT_EXACT, MODE_REDUNDANCY, MODE_TEST,
    ROB_VERSION,
};
use rob_engine::{Engine, EngineConfig, InputFlow, OutputFlow};
use rob_matrix::{MatrixConfig, Node};
use rob_node::flow::{FlowFormat, Protection};
use rob_node::packetize::Packetizer;

/// The program's heap, backed by the Core's own first-fit arena: `alloc` (5) and
/// `dealloc` (6) hand a block over and take it back. The arena is the Core's;
/// the program only ever borrows from it, which is the capability model's
/// "no ambient authority" applied to memory.
struct CoreAlloc;

unsafe impl GlobalAlloc for CoreAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // The Core's first-fit arena aligns to 16 bytes; the engine's Vec/Box
        // allocations never need more than 8, so 16 covers them. A request for
        // a stricter alignment is refused rather than silently mis-aligned.
        if layout.align() > 16 {
            return core::ptr::null_mut();
        }
        let size = layout.size().max(1);
        let mut ret: u64;
        unsafe {
            asm!(
                "svc #0",
                inout("x0") 5u64 => ret,
                in("x1") size as u64,
                options(nostack)
            );
        }
        ret as *mut u8
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        unsafe {
            asm!(
                "svc #0",
                inout("x0") 6u64 => _,
                in("x1") ptr as u64,
                options(nostack)
            );
        }
    }
}

#[global_allocator]
static ALLOC: CoreAlloc = CoreAlloc;

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    loop {
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}

/// The payload a show's audio rides in (placeholder: a ramp; the DSP path and
/// the plugins are deferred by Rob). 48 bytes = 16 mono 24-bit samples at 1 ms.
const PAYLOAD_BYTES: usize = 48;

// ---------------------------------------------------------------------------
// The six RTP tests rob-core runs in `cargo test`, ported as no_std self-checks.
// They call the real `rtp::write_header` and `rtp::parse` — the same code the
// engine uses — so a pass here means the wire format round-trips on the Core.
// ---------------------------------------------------------------------------

fn sample() -> (RtpHeader, RobCore) {
    (
        RtpHeader {
            marker: true,
            payload_type: 97,
            sequence: 0xFFFE,
            timestamp: 0xDEAD_BEEF,
            ssrc: 0x1234_5678,
        },
        RobCore {
            version: ROB_VERSION,
            mode: MODE_REDUNDANCY | MODE_TEST,
            path_id: 3,
            slot: 0xABC,
            budget_us: 7_500,
            send_time_us: 0x0102_0304,
        },
    )
}

fn check_header_with_extension_round_trips() -> bool {
    let (h, c) = sample();
    let mut buf = [0u8; 64];
    let Ok(n) = rtp::write_header(&mut buf, &h, Some((DEFAULT_ROB_EXT_ID, &c))) else {
        return false;
    };
    if n != 28 {
        return false; // spec: RTP + rob-core = 28 bytes
    }
    buf[n..n + 4].copy_from_slice(b"DATA");
    let Ok(p) = rtp::parse(&buf[..n + 4], DEFAULT_ROB_EXT_ID) else {
        return false;
    };
    p.header == h && p.rob == Some(c) && p.payload == b"DATA"
}

fn check_plain_rtp_has_no_rob_fields() -> bool {
    let (h, _) = sample();
    let mut buf = [0u8; 16];
    let Ok(n) = rtp::write_header(&mut buf, &h, None) else {
        return false;
    };
    if n != 12 {
        return false;
    }
    let Ok(p) = rtp::parse(&buf[..n], DEFAULT_ROB_EXT_ID) else {
        return false;
    };
    p.rob.is_none() && p.payload.is_empty()
}

fn check_other_extension_ids_are_skipped() -> bool {
    let (h, c) = sample();
    let mut buf = [0u8; 64];
    let Ok(n) = rtp::write_header(&mut buf, &h, Some((5, &c))) else {
        return false;
    };
    // element 5 is not rob-core when we asked for id 1
    let Ok(p) = rtp::parse(&buf[..n], DEFAULT_ROB_EXT_ID) else {
        return false;
    };
    p.rob.is_none()
}

fn check_rejects_out_of_range_fields() -> bool {
    let (h, mut c) = sample();
    c.slot = 0x1000;
    let mut buf = [0u8; 64];
    matches!(
        rtp::write_header(&mut buf, &h, Some((1, &c))),
        Err(rob_core::Error::FieldRange)
    )
}

fn check_rejects_truncated_and_wrong_version() -> bool {
    if !matches!(rtp::parse(&[0x80; 5], 1), Err(rob_core::Error::TooShort)) {
        return false;
    }
    let mut buf = [0u8; 12];
    buf[0] = 0x40; // version 1
    matches!(rtp::parse(&buf, 1), Err(rob_core::Error::BadVersion))
}

fn check_rtp_padding_is_removed() -> bool {
    let (h, _) = sample();
    let mut buf = [0u8; 20];
    let Ok(n) = rtp::write_header(&mut buf, &h, None) else {
        return false;
    };
    buf[0] |= 0x20; // padding bit
    buf[n..n + 2].copy_from_slice(b"AB");
    buf[n + 2..n + 4].copy_from_slice(&[0, 2]); // 2 bytes of padding
    let Ok(p) = rtp::parse(&buf[..n + 4], 1) else {
        return false;
    };
    p.payload == b"AB"
}

fn check_allocator() -> bool {
    // The program's heap, backed by the Core's arena: allocate (with Vec growth,
    // so several blocks), write, read back, and free on drop. A pass means the
    // program has real memory the engine's Vec/Box can use.
    let mut v: Vec<u32> = Vec::new();
    for i in 0..64u32 {
        v.push(i);
    }
    v.len() == 64 && v[0] == 0 && v[63] == 63 && v.iter().sum::<u32>() == 2016
}

fn run_self_checks() -> usize {
    let mut passed: usize = 0;
    if check_header_with_extension_round_trips() {
        passed += 1;
    }
    if check_plain_rtp_has_no_rob_fields() {
        passed += 1;
    }
    if check_other_extension_ids_are_skipped() {
        passed += 1;
    }
    if check_rejects_out_of_range_fields() {
        passed += 1;
    }
    if check_rejects_truncated_and_wrong_version() {
        passed += 1;
    }
    if check_rtp_padding_is_removed() {
        passed += 1;
    }
    if check_allocator() {
        passed += 1;
    }
    if cfg!(feature = "selfcheck-mutant-wrong-count") {
        // The mutation: report one fewer than actually passed, so the gate's
        // "7/7" check must go red.
        passed.saturating_sub(1)
    } else {
        passed
    }
}

/// One millisecond in the Core's clock (ns), and the engine's period in frames.
const MS: u64 = 1_000_000;
const P: i64 = 96;

/// The mix engine's output flow SSRC — distinct from the self-check packet's
/// 0xC0FFEE00, so the capture can attribute this RTP to the engine. The
/// mutation makes the stream unattributable, exactly what the gate's engine
/// check must catch.
const ENGINE_OUT_SSRC: u32 = if cfg!(feature = "engine-mutant-wrong-ssrc") {
    0
} else {
    0x0000_000B
};

/// Send a packet the engine produced through the Core's `send_rtp` syscall.
fn send_rtp_bytes(bytes: &[u8]) {
    unsafe {
        asm!(
            "svc #0",
            inout("x0") 4u64 => _,
            in("x1") bytes.as_ptr() as u64,
            in("x2") bytes.len() as u64,
            options(nostack)
        );
    }
}

/// Instantiate a minimal mix engine (one stereo input, one stereo output,
/// passthrough routing), feed it four synthesized periods, and send every mixed
/// output packet out through `send_rtp`. Returns the number of packets the
/// engine produced.
fn run_engine() -> usize {
    let fmt = FlowFormat::new(Protection::Duplicate);
    let mut inputs = Vec::new();
    inputs.push(InputFlow {
        ssrc: 0xA,
        channels: [0, 1],
        pre_channels: [0, 1],
    });
    let mut outputs = Vec::new();
    outputs.push(OutputFlow {
        ssrc: ENGINE_OUT_SSRC,
        correction_ssrc: 0xC,
        channels: [0, 1],
    });
    let cfg = EngineConfig {
        fmt,
        inputs,
        outputs,
        mix_delay_ns: 3 * MS,
        latency_ns: 40 * MS,
        payload_type: 97,
        ext_id: 1,
        jitter_periods: 8,
        repeat_corrections: false,
        sender_clock_inputs: false,
    };
    // A minimal matrix: the Core's arena is 256 KiB, and the shipped defaults
    // (16 384 crosspoints, a 4 096-entry queue) would not fit. Eight crosspoints
    // and a 64-entry queue are the honest size for a one-in, one-out engine.
    let mcfg = MatrixConfig {
        inputs: 2,
        outputs: 2,
        max_buses: 2,
        max_crosspoints: 8,
        max_block: 256,
        ramp_frames: 48,
        queue: 64,
        correction_records: 8,
        correction_span: 256,
    };
    let mut eng = Engine::new(cfg, mcfg);
    {
        let m = eng.matrix();
        // Passthrough: input 0 -> output 0, input 1 -> output 1.
        let _ = m.connect(Node::Input(0), Node::Output(0), 0.0);
        let _ = m.connect(Node::Input(1), Node::Output(1), 0.0);
    }
    let mut sender = Packetizer::new(fmt, 0xA, 97, 1);
    let mut pkt = [0u8; 1500];
    let mut pcm = [0i32; (P * 2) as usize];
    let mut emitted = 0usize;
    let mut now = 100 * MS;
    for k in 0..4i64 {
        let first = k * P;
        for j in 0..P as usize {
            for c in 0..2usize {
                // A ramp in the low bits — clearly ours, no float math needed.
                pcm[j * 2 + c] = ((first + j as i64) * 1_000) as i32;
            }
        }
        let Ok(b) = sender.build(first, &pcm, now, now + 20 * MS, 0, 0, &mut pkt) else {
            return emitted;
        };
        eng.on_datagram(&pkt[..b.len], now);
        sender.advance(first, &pcm);
        eng.run_due(now, |bytes| {
            send_rtp_bytes(bytes);
            emitted += 1;
        });
        now += 2 * MS;
    }
    emitted
}

/// The entry the loader calls. It makes the four syscalls a loadable program is
/// allowed to make, runs rob-core's six RTP tests as no_std self-checks, puts
/// the pass count in the RTP payload, sends its own AMT/ROB-shaped RTP packet
/// (built with `rtp::write_header`), runs the mix engine, and returns 66.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> u64 {
    unsafe {
        // report (0)
        asm!("svc #0", inout("x0") 0u64 => _, options(nostack));
        // apply_certified (1, arg 42)
        asm!("svc #0", inout("x0") 1u64 => _, inout("x1") 42u64 => _, options(nostack));
        // yield (2)
        asm!("svc #0", inout("x0") 2u64 => _, options(nostack));
        // send_udp (3)
        asm!("svc #0", inout("x0") 3u64 => _, options(nostack));

        // send_rtp (4): the engine's own RTP packet, built here and handed to
        // the Core to transmit. x1 = pointer, x2 = length. The payload's first
        // three bytes carry the self-check result ("SC" + pass count) so the
        // gate reads the tests' verdict off the CAPTURE, not the console.
        // Run the six RTP self-checks FIRST, before the packet buffer exists, so
        // their stack frames cannot alias the packet's slot.
        let passed = run_self_checks();
        let mut packet = [0u8; 12 + 16 + PAYLOAD_BYTES];
        let header = RtpHeader {
            marker: false,
            payload_type: 96,
            sequence: 100,
            timestamp: 4800,
            // The mutation makes the stream unattributable, exactly the defect
            // the gate's engine-RTP check must catch (the same way the Core's
            // demo RTP has rtp-mutant-wrong-ssrc).
            ssrc: if cfg!(feature = "rtp-mutant-wrong-ssrc") {
                0
            } else {
                0xC0FF_EE00
            },
        };
        let core = RobCore {
            version: ROB_VERSION,
            mode: MODE_BIT_EXACT,
            path_id: 0,
            slot: 0,
            budget_us: 0,
            send_time_us: 0,
        };
        // The element id is negotiated in SDP (`a=extmap`); 1 is the default.
        let hlen = match rtp::write_header(&mut packet, &header, Some((1, &core))) {
            Ok(n) => n,
            Err(_) => return u64::MAX,
        };
        packet[hlen] = 0x53; // 'S'
        packet[hlen + 1] = 0x43; // 'C'
        packet[hlen + 2] = passed as u8;
        let len = hlen + PAYLOAD_BYTES;
        let ptr = packet.as_ptr() as u64;
        asm!(
            "svc #0",
            inout("x0") 4u64 => _,
            in("x1") ptr,
            in("x2") len as u64,
            options(nostack)
        );

        // The mix engine itself: instantiate, feed four periods, mix, and send
        // every output packet through send_rtp. The gate reads the engine's
        // output SSRC off the capture.
        let _mixed = run_engine();
    }
    66
}
