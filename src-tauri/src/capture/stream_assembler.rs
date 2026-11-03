use std::collections::VecDeque;

use super::framing::{resync_start, RESYNC_SEARCH};
use super::packet_accumulator::PacketAccumulator;
use super::stream_processor::StreamProcessor;

/// Reassembles TCP stream fragments and feeds complete data to the StreamProcessor.
pub struct StreamAssembler {
    accumulator: PacketAccumulator,
    /// Bytes were lost before what comes next (`tcp_order`): read nothing
    /// until a frame starts.
    after_gap: bool,
    /// Length and original capture time of each contiguous byte range.
    /// Unknown timing is never replaced with a later chunk's timestamp.
    capture_spans: VecDeque<(usize, Option<i64>)>,
}

const MAX_CAPTURE_SPANS: usize = 4096;

#[cfg(test)]
#[path = "stream_assembler_tests.rs"]
mod tests;

impl StreamAssembler {
    pub fn new() -> Self {
        Self {
            accumulator: PacketAccumulator::new(),
            after_gap: false,
            capture_spans: VecDeque::new(),
        }
    }

    /// Process a TCP chunk. Returns true if any packets were successfully parsed.
    pub fn process_chunk(&mut self, data: &[u8], processor: &mut StreamProcessor) -> bool {
        self.process_chunk_at(data, processor, None)
    }

    /// Live input retains capture times through segmentation and reordering.
    /// Other callers keep the untimed API and cannot contribute live ping.
    pub fn process_chunk_at(&mut self, data: &[u8], processor: &mut StreamProcessor, captured_at: Option<i64>) -> bool {
        let before = self.accumulator.size();
        self.accumulator.append(data);
        if self.accumulator.size() != before + data.len() {
            self.capture_spans.clear(); // the accumulator hit its byte limit
        }
        if !data.is_empty() {
            match self.capture_spans.back_mut() {
                Some((length, at)) if *at == captured_at => *length += data.len(),
                _ => self.capture_spans.push_back((data.len(), captured_at)),
            }
        }
        if self.capture_spans.len() > MAX_CAPTURE_SPANS {
            self.capture_spans.clear();
            self.capture_spans.push_back((self.accumulator.size(), None));
        }
        let mut parsed_any = false;

        if self.after_gap {
            match resync_start(self.accumulator.snapshot()) {
                Some(start) => {
                    self.discard_bytes(start);
                    self.after_gap = false;
                }
                // Nowhere yet: wait for more, unless the search has seen all
                // it looks at, then read on as before.
                None if self.accumulator.size() < 2 * RESYNC_SEARCH => return false,
                None => self.after_gap = false,
            }
        }

        loop {
            if self.accumulator.size() == 0 {
                break;
            }

            let snapshot = self.accumulator.snapshot().to_vec();
            let consumed = processor.consume_stream_timed(&snapshot, |start, end| self.captured_at(start, end));

            if consumed > 0 {
                self.discard_bytes(consumed);
                parsed_any = true;
            } else {
                break;
            }
        }

        parsed_any
    }

    /// Bytes were lost from the stream here: the frame being read lost its
    /// rest (or its start), so it goes, and reading starts again at the next
    /// whole frame.
    pub fn lose_bytes(&mut self) {
        let size = self.accumulator.size();
        self.discard_bytes(size);
        self.after_gap = true;
    }

    fn discard_bytes(&mut self, length: usize) {
        self.accumulator.discard_bytes(length);
        let mut left = length;
        while let Some((count, _)) = self.capture_spans.front_mut() {
            if *count > left {
                *count -= left;
                break;
            }
            left -= *count;
            self.capture_spans.pop_front();
        }
    }

    /// A frame is complete when all of its bytes have arrived, including
    /// when its tail arrived before its header. Ignore neighboring frames.
    fn captured_at(&self, start: usize, end: usize) -> Option<i64> {
        let mut offset = 0;
        let mut latest = None;
        for &(length, at) in &self.capture_spans {
            let next = offset + length;
            if next > start && offset < end {
                let at = at?;
                latest = Some(latest.map_or(at, |old: i64| old.max(at)));
            }
            if next >= end { return latest; }
            offset = next;
        }
        None
    }

    pub fn buffered_bytes(&self) -> usize {
        self.accumulator.size()
    }
}
