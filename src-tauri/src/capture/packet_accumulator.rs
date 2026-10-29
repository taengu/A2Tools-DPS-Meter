/// Growable byte buffer for TCP stream reassembly.
/// Appends incoming chunks and allows the parser to consume bytes from the front.
pub struct PacketAccumulator {
    buffer: Vec<u8>,
}

const MAX_BUFFER_SIZE: usize = 2 * 1024 * 1024;

impl PacketAccumulator {
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(64 * 1024),
        }
    }

    /// Returns true when bytes had to be dropped, so the caller can resync.
    pub fn append(&mut self, data: &[u8]) -> bool {
        let overflow = data.len() > MAX_BUFFER_SIZE - self.buffer.len();
        if overflow {
            tracing::error!("PacketAccumulator buffer exceeded limit, resetting");
            self.buffer.clear();
        }
        // A single oversized input must not allocate beyond the same limit.
        if data.len() <= MAX_BUFFER_SIZE {
            let needed = self.buffer.len() + data.len();
            if needed > self.buffer.capacity() {
                // Vec's automatic doubling could reserve more than the cap
                // even when the bytes themselves fit. Grow geometrically,
                // but clamp the allocation too.
                let capacity = needed.max(self.buffer.capacity().saturating_mul(2)).min(MAX_BUFFER_SIZE);
                self.buffer.reserve_exact(capacity - self.buffer.len());
            }
            self.buffer.extend_from_slice(data);
        }
        overflow
    }

    pub fn snapshot(&self) -> &[u8] {
        &self.buffer
    }

    pub fn discard_bytes(&mut self, length: usize) {
        if length >= self.buffer.len() {
            self.buffer.clear();
        } else {
            self.buffer.drain(..length);
        }
    }

    pub fn size(&self) -> usize {
        self.buffer.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chunk_cannot_push_the_buffer_past_its_limit() {
        let mut buffer = PacketAccumulator::new();
        assert!(!buffer.append(&vec![1; MAX_BUFFER_SIZE]));
        assert_eq!(buffer.size(), MAX_BUFFER_SIZE);
        assert!(buffer.append(&[2, 3]));
        assert_eq!(buffer.snapshot(), &[2, 3]);
        assert!(!buffer.append(&[4]));
        assert_eq!(buffer.snapshot(), &[2, 3, 4]);
    }

    #[test]
    fn a_single_oversized_chunk_is_dropped_without_growing_the_buffer() {
        let mut buffer = PacketAccumulator::new();
        buffer.append(&[1]);
        let capacity = buffer.buffer.capacity();
        assert!(buffer.append(&vec![2; MAX_BUFFER_SIZE + 1]));
        assert_eq!(buffer.size(), 0);
        assert_eq!(buffer.buffer.capacity(), capacity);
        assert!(!buffer.append(&[3]));
        assert_eq!(buffer.snapshot(), &[3]);
    }

    #[test]
    fn growth_from_an_uneven_chunk_cannot_double_past_the_limit() {
        let mut buffer = PacketAccumulator::new();
        assert!(!buffer.append(&vec![1; MAX_BUFFER_SIZE / 2 + 1]));
        assert!(!buffer.append(&vec![2; MAX_BUFFER_SIZE / 2 - 1]));
        assert_eq!(buffer.size(), MAX_BUFFER_SIZE);
        assert!(buffer.buffer.capacity() <= MAX_BUFFER_SIZE);
    }
}
