use super::framing::{resync_start, RESYNC_SEARCH};
use super::packet_accumulator::PacketAccumulator;
use super::stream_processor::StreamProcessor;

/// Reassembles TCP stream fragments and feeds complete data to the StreamProcessor.
pub struct StreamAssembler {
    accumulator: PacketAccumulator,
    /// Bytes were lost before what comes next (`tcp_order`): read nothing
    /// until a frame starts.
    after_gap: bool,
}

impl StreamAssembler {
    pub fn new() -> Self {
        Self {
            accumulator: PacketAccumulator::new(),
            after_gap: false,
        }
    }

    /// Process a TCP chunk. Returns true if any packets were successfully parsed.
    pub fn process_chunk(&mut self, data: &[u8], processor: &mut StreamProcessor) -> bool {
        if self.accumulator.append(data) {
            self.after_gap = true;
        }
        let mut parsed_any = false;

        if self.after_gap {
            match resync_start(self.accumulator.snapshot()) {
                Some(start) => {
                    self.accumulator.discard_bytes(start);
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
            let consumed = processor.consume_stream(&snapshot);

            if consumed > 0 {
                self.accumulator.discard_bytes(consumed);
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
        self.accumulator.discard_bytes(size);
        self.after_gap = true;
    }

    pub fn buffered_bytes(&self) -> usize {
        self.accumulator.size()
    }
}
