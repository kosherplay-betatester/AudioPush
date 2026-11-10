package com.kosherplay.audiopush

import java.util.concurrent.atomic.AtomicInteger

/**
 * Lock-free single-producer / single-consumer ring of fixed-size audio blocks.
 *
 * Storage is allocated once; push and pop copy into and out of slots that
 * already exist, so neither side ever asks the allocator for anything. On
 * Android that matters twice over: an allocation in the player thread can
 * trigger a GC pause, and a GC pause is a dropout.
 *
 * Contract: one thread pushes, one thread pops/trims.
 */
class BlockRing(capacityBlocks: Int) {
    private val slots = Array(capacityBlocks) { ShortArray(Config.BLOCK_SAMPLES) }
    private val mask = capacityBlocks - 1
    private val head = AtomicInteger(0)
    private val tail = AtomicInteger(0)

    init {
        require(capacityBlocks > 0 && (capacityBlocks and mask) == 0) {
            "ring capacity must be a power of two"
        }
    }

    fun size(): Int = head.get() - tail.get()

    /** Producer side. Returns false when full; the caller drops the block. */
    fun push(block: ShortArray): Boolean {
        val h = head.get()
        if (h - tail.get() >= slots.size) return false
        System.arraycopy(block, 0, slots[h and mask], 0, Config.BLOCK_SAMPLES)
        head.set(h + 1)
        return true
    }

    /** Consumer side. Returns false when empty. */
    fun pop(out: ShortArray): Boolean {
        val t = tail.get()
        if (head.get() == t) return false
        System.arraycopy(slots[t and mask], 0, out, 0, Config.BLOCK_SAMPLES)
        tail.set(t + 1)
        return true
    }

    /** Consumer side. Drops the oldest blocks until at most [keep] remain. */
    fun trimTo(keep: Int): Int {
        var dropped = 0
        while (head.get() - tail.get() > keep) {
            tail.set(tail.get() + 1)
            dropped++
        }
        return dropped
    }
}
