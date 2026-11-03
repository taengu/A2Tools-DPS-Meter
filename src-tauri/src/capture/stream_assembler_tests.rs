use super::*;
use std::sync::Arc;
use crate::capture::tcp_order::{Piece, TcpOrder};
use crate::combat::{data_storage::DataStorage, ping_tracker::PingTracker};
use crate::i18n::lookup::{NpcLookup, SkillLookup};

const NOW: i64 = 1_790_000_000_000;

fn frame(payload: &[u8]) -> Vec<u8> {
    let mut length = payload.len() + 4;
    let mut out = Vec::new();
    while length >= 128 { out.push((length as u8 & 0x7f) | 0x80); length >>= 7; }
    out.push(length as u8);
    out.extend_from_slice(payload);
    out
}

fn response(sent: i64) -> Vec<u8> {
    let mut payload = vec![3, 0x36, 0, 0];
    payload.extend_from_slice(&(sent + 62_135_596_800_000).to_le_bytes());
    payload.extend_from_slice(&0i64.to_le_bytes());
    frame(&payload)
}

fn setup() -> (StreamAssembler, StreamProcessor, Arc<PingTracker>) {
    let ping = Arc::new(PingTracker::new());
    let mut p = StreamProcessor::new(Arc::new(DataStorage::new()), Arc::new(SkillLookup::new()), Arc::new(NpcLookup::new()));
    p.set_ping_observer(Some(ping.clone()));
    (StreamAssembler::new(), p, ping)
}

fn feed(order: &mut TcpOrder, a: &mut StreamAssembler, p: &mut StreamProcessor, seq: u32, data: &[u8], at: i64) {
    for (piece, received_at) in order.push_timed(seq, data, at) {
        match piece {
            Piece::Gap { .. } => a.lose_bytes(),
            Piece::Bytes(bytes) => { a.process_chunk_at(&bytes, p, Some(received_at)); }
        }
    }
}

#[test]
fn ping_responses_split_at_every_boundary_retain_their_completion_time() {
    let reply = response(NOW);
    for split in 1..reply.len() {
        let (mut a, mut p, ping) = setup();
        p.set_override_timestamp(Some(NOW + 9_000));
        a.process_chunk_at(&reply[..split], &mut p, Some(NOW + 20));
        assert_eq!(ping.current_ping_ms(), None);
        a.process_chunk_at(&reply[split..], &mut p, Some(NOW + 60));
        assert_eq!(ping.current_ping_ms(), Some(60), "split {split}; combat clock must not become RTT");
        assert!(a.capture_spans.is_empty());
        p.set_override_timestamp(None);
    }
}

#[test]
fn ping_ignores_embedded_body_markers_even_when_segmented_and_coalesced() {
    let mut body = vec![0xee, 0x36, 0xaa];
    body.extend_from_slice(&response(NOW));
    body.resize(140, 0xbb); // a two-byte length prefix
    let fake = frame(&body);
    for split in 1..fake.len() {
        let (mut a, mut p, ping) = setup();
        for n in 0..8 { a.process_chunk_at(&response(NOW - (8 - n) * 10_000), &mut p, Some(NOW - (8 - n) * 10_000 + 60)); }
        a.process_chunk_at(&fake[..split], &mut p, Some(NOW + 9_000));
        let rest = [&fake[split..], &response(NOW + 10_000)].concat();
        a.process_chunk_at(&rest, &mut p, Some(NOW + 10_060));
        assert_eq!(ping.current_ping_ms(), Some(60));
        assert_eq!(ping.current_jitter_ms(), Some(0.0), "embedded frame at split {split}");
    }
}

#[test]
fn ping_inside_a_compressed_bundle_uses_the_bundle_completion_time() {
    let inner = [frame(&[0xee, 0x36, 0, 0]), response(NOW)].concat();
    let mut payload = vec![0xff, 0xff];
    payload.extend_from_slice(&(inner.len() as u32).to_le_bytes());
    payload.extend_from_slice(&lz4_flex::block::compress(&inner));
    let bundle = frame(&payload);
    let (mut a, mut p, ping) = setup();
    a.process_chunk_at(&bundle[..8], &mut p, Some(NOW + 10));
    a.process_chunk_at(&bundle[8..], &mut p, Some(NOW + 70));
    assert_eq!(ping.current_ping_ms(), Some(70));
}

#[test]
fn ping_tail_before_header_uses_latest_arrival_and_retransmissions_add_nothing() {
    let (mut a, mut p, ping) = setup();
    let mut order = TcpOrder::default();
    let seed = frame(&[0xee, 0x36, 0, 0]);
    feed(&mut order, &mut a, &mut p, 1000, &seed, NOW);
    let start = 1000 + seed.len() as u32;
    let reply = response(NOW);
    feed(&mut order, &mut a, &mut p, start + 10, &reply[10..], NOW + 40);
    assert_eq!(ping.current_ping_ms(), None);
    feed(&mut order, &mut a, &mut p, start, &reply[..10], NOW + 60);
    assert_eq!(ping.current_ping_ms(), Some(60));
    feed(&mut order, &mut a, &mut p, start, &reply, NOW + 200);
    assert_eq!(ping.current_ping_ms(), Some(60));
    assert_eq!(ping.current_jitter_ms(), None);
}

#[test]
fn ping_held_behind_an_unrelated_gap_keeps_its_original_capture_time() {
    let (mut a, mut p, ping) = setup();
    let mut order = TcpOrder::default();
    let other = frame(&[0xee, 0x36, 0, 0]);
    feed(&mut order, &mut a, &mut p, 1000, &other, NOW);
    let gap = 1000 + other.len() as u32;
    let reply = response(NOW);
    feed(&mut order, &mut a, &mut p, gap + other.len() as u32, &reply, NOW + 60);
    feed(&mut order, &mut a, &mut p, gap, &other, NOW + 210);
    assert_eq!(ping.current_ping_ms(), Some(60), "the late preceding frame is not ping latency");
}

#[test]
fn later_frame_completion_does_not_retime_a_coalesced_response() {
    let (mut a, mut p, ping) = setup();
    let other = frame(&[0xee, 0x36, 0, 0]);
    a.process_chunk_at(&[response(NOW), other[..2].to_vec()].concat(), &mut p, Some(NOW + 60));
    assert_eq!(a.buffered_bytes(), 2);
    assert_eq!(a.capture_spans.iter().map(|s| s.0).sum::<usize>(), 2);
    a.process_chunk_at(&other[2..], &mut p, Some(NOW + 2_000));
    assert_eq!(ping.current_ping_ms(), Some(60));
    assert!(a.capture_spans.is_empty());
}

#[test]
fn gap_filling_retransmission_preserves_an_already_received_responses_time() {
    let (mut a, mut p, ping) = setup();
    let mut order = TcpOrder::default();
    let other = frame(&[0xee, 0x36, 0, 0]);
    feed(&mut order, &mut a, &mut p, 1000, &other, NOW);
    let gap = 1000 + other.len() as u32;
    let reply = response(NOW);
    feed(&mut order, &mut a, &mut p, gap + other.len() as u32, &reply, NOW + 60);
    feed(&mut order, &mut a, &mut p, gap, &[other, reply].concat(), NOW + 210);
    assert_eq!(ping.current_ping_ms(), Some(60));
}

#[test]
fn overlapping_queued_segments_preserve_the_earliest_response_copy() {
    let (mut a, mut p, ping) = setup();
    let mut order = TcpOrder::default();
    let other = frame(&[0xee, 0x36, 0, 0]);
    feed(&mut order, &mut a, &mut p, 1000, &other, NOW);
    let gap = 1000 + other.len() as u32;
    let reply = response(NOW);
    feed(&mut order, &mut a, &mut p, gap + other.len() as u32, &reply, NOW + 60);
    feed(&mut order, &mut a, &mut p, gap + 3, &[&other[3..], &reply].concat(), NOW + 100);
    feed(&mut order, &mut a, &mut p, gap, &other[..3], NOW + 210);
    assert_eq!(ping.current_ping_ms(), Some(60));
}

#[test]
fn missing_timing_and_unobserved_processors_never_mutate_live_ping() {
    let (mut a, mut p, ping) = setup();
    let reply = response(NOW);
    a.process_chunk(&reply[..10], &mut p);
    a.process_chunk_at(&reply[10..], &mut p, Some(NOW + 60));
    assert_eq!(ping.current_ping_ms(), None);
    p.set_ping_observer(None);
    a.process_chunk_at(&reply, &mut p, Some(NOW + 60));
    assert_eq!(ping.current_ping_ms(), None);
    p.set_ping_observer(Some(ping.clone()));
    a.process_chunk_at(&reply[..10], &mut p, Some(NOW + 60));
    a.lose_bytes();
    assert!(a.capture_spans.is_empty());
    ping.reset();
    let replies = [response(NOW + 1_000), response(NOW + 1_010)].concat();
    a.process_chunk_at(&replies, &mut p, Some(NOW + 1_070));
    assert_eq!(ping.current_ping_ms(), Some(60));
    assert_eq!(ping.current_jitter_ms(), Some(10.0));
}

#[test]
fn timing_metadata_is_bounded_and_accumulator_reset_drops_old_spans() {
    let (mut a, mut p, ping) = setup();
    a.after_gap = true;
    a.accumulator.append(&vec![0; MAX_CAPTURE_SPANS]);
    a.capture_spans.extend((0..MAX_CAPTURE_SPANS).map(|i| (1, Some(i as i64))));
    a.process_chunk_at(&[0], &mut p, Some(NOW));
    assert_eq!(a.capture_spans, VecDeque::from([(MAX_CAPTURE_SPANS + 1, None)]));
    a.lose_bytes();
    a.after_gap = false;
    let old = 2 * 1024 * 1024 + 1;
    a.accumulator.append(&vec![0; old]);
    a.capture_spans.push_back((old, Some(NOW + 9_000)));
    a.process_chunk_at(&response(NOW), &mut p, Some(NOW + 60));
    assert_eq!(ping.current_ping_ms(), Some(60));
    assert!(a.capture_spans.is_empty());
}
