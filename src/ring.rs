//! A lock-free single-producer / single-consumer ring of fixed-size audio blocks.
//!
//! All storage is allocated once, up front. `push` and `pop` copy samples into
//! and out of slots that already exist, so neither side ever touches the
//! allocator; that is what keeps the audio thread free of surprise pauses.
//!
//! Safety contract: exactly one thread may call `push` (the producer) and
//! exactly one may call `pop`/`trim_to`/`clear` (the consumer). `len` is safe
//! from either side.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::config::BLOCK_SAMPLES;

pub struct BlockRing {
    /// Pre-allocated slots. `UnsafeCell` because producer and consumer touch
    /// disjoint slots, which the borrow checker cannot see.
    slots: Box<[UnsafeCell<[i16; BLOCK_SAMPLES]>]>,
    mask: usize,
    /// Next slot the producer will write (monotonically increasing).
    head: AtomicUsize,
    /// Next slot the consumer will read (monotonically increasing).
    tail: AtomicUsize,
}

// Each slot is only ever touched by one side at a time; the head/tail
// acquire-release pairing below is what establishes that.
unsafe impl Send for BlockRing {}
unsafe impl Sync for BlockRing {}

impl BlockRing {
    /// `capacity_blocks` must be a power of two.
    pub fn new(capacity_blocks: usize) -> Self {
        assert!(
            capacity_blocks.is_power_of_two(),
            "ring capacity must be a power of two"
        );
        let mut slots = Vec::with_capacity(capacity_blocks);
        for _ in 0..capacity_blocks {
            slots.push(UnsafeCell::new([0i16; BLOCK_SAMPLES]));
        }
        BlockRing {
            slots: slots.into_boxed_slice(),
            mask: capacity_blocks - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// Number of blocks currently queued.
    pub fn len(&self) -> usize {
        self.head
            .load(Ordering::Acquire)
            .wrapping_sub(self.tail.load(Ordering::Acquire))
    }

    /// Producer side. Copies one block in. Returns false when the ring is full
    /// (the caller then drops the block rather than block the audio thread).
    pub fn push(&self, block: &[i16; BLOCK_SAMPLES]) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head.wrapping_sub(tail) >= self.slots.len() {
            return false;
        }
        // Safe: this slot sits beyond the consumer tail, so only we touch it.
        unsafe {
            let dst = &mut *self.slots[head & self.mask].get();
            dst.copy_from_slice(block);
        }
        self.head.store(head.wrapping_add(1), Ordering::Release);
        true
    }

    /// Consumer side. Copies one block out. Returns false when empty.
    pub fn pop(&self, out: &mut [i16; BLOCK_SAMPLES]) -> bool {
        let tail = self.tail.load(Ordering::Relaxed);
        if self.head.load(Ordering::Acquire) == tail {
            return false;
        }
        unsafe {
            let src = &*self.slots[tail & self.mask].get();
            out.copy_from_slice(src);
        }
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        true
    }

    /// Consumer side. Throws away the oldest blocks until at most `keep`
    /// remain. Stops latency creeping upward after a network burst.
    pub fn trim_to(&self, keep: usize) -> usize {
        let mut dropped = 0;
        loop {
            let tail = self.tail.load(Ordering::Relaxed);
            let head = self.head.load(Ordering::Acquire);
            if head.wrapping_sub(tail) <= keep {
                break;
            }
            self.tail.store(tail.wrapping_add(1), Ordering::Release);
            dropped += 1;
        }
        dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_pop_roundtrip() {
        let ring = BlockRing::new(4);
        let mut block = [0i16; BLOCK_SAMPLES];
        block[0] = 1234;
        assert!(ring.push(&block));
        assert_eq!(ring.len(), 1);
        let mut out = [0i16; BLOCK_SAMPLES];
        assert!(ring.pop(&mut out));
        assert_eq!(out[0], 1234);
        assert_eq!(ring.len(), 0);
        assert!(!ring.pop(&mut out));
    }

    #[test]
    fn full_ring_rejects_push() {
        let ring = BlockRing::new(2);
        let block = [0i16; BLOCK_SAMPLES];
        assert!(ring.push(&block));
        assert!(ring.push(&block));
        assert!(!ring.push(&block));
    }

    #[test]
    fn trim_keeps_newest() {
        let ring = BlockRing::new(8);
        for i in 0..6 {
            let mut b = [0i16; BLOCK_SAMPLES];
            b[0] = i;
            ring.push(&b);
        }
        assert_eq!(ring.trim_to(2), 4);
        let mut out = [0i16; BLOCK_SAMPLES];
        ring.pop(&mut out);
        assert_eq!(out[0], 4);
    }
}
