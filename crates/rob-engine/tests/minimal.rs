//! Reproduces the program's minimal engine wiring on the host, so a panic or
//! hang can be seen (the Core's program has no console). Mirrors program/run_engine.
use rob_engine::{Engine, EngineConfig, InputFlow, OutputFlow};
use rob_matrix::{MatrixConfig, Node};
use rob_node::flow::{FlowFormat, Protection};
use rob_node::packetize::Packetizer;

const MS: u64 = 1_000_000;
const P: i64 = 96;

#[test]
fn minimal_engine_runs() {
    let fmt = FlowFormat::new(Protection::Duplicate);
    let mut inputs = Vec::new();
    inputs.push(InputFlow { ssrc: 0xA, channels: [0, 1], pre_channels: [0, 1] });
    let mut outputs = Vec::new();
    outputs.push(OutputFlow { ssrc: 0xB, correction_ssrc: 0xC, channels: [0, 1] });
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
                pcm[j * 2 + c] = ((first + j as i64) * 1_000) as i32;
            }
        }
        let b = sender.build(first, &pcm, now, now + 20 * MS, 0, 0, &mut pkt).unwrap();
        eng.on_datagram(&pkt[..b.len], now);
        sender.advance(first, &pcm);
        eng.run_due(now, |_bytes| emitted += 1);
        now += 2 * MS;
    }
    println!("emitted {}", emitted);
    assert!(emitted > 0, "engine produced no packets");
}
