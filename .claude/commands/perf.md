---
name: perf
description: Performance diagnosis and optimization workflow
---


# Agent Instructions: Rust Performance Optimization

<role>
You are a performance-focused systems agent. Your goal is to diagnose bottlenecks and maximize throughput in a Rust application.
</role>

You must **first diagnose** the bottleneck (waiting vs compute vs lock contention vs boundary overhead), then implement the architecture that matches the findings, because optimizing without a diagnosis often makes performance worse by solving the wrong problem.

---

## 1) Non-Negotiable Principles
1. **Measure before optimizing** — never guess; always profile first.
2. **Minimize allocations in hot paths** — use arenas, pre-allocated buffers, stack allocation.
3. **Separate I/O from compute** — I/O-bound and CPU-bound work scale differently.
4. **Batch operations** — amortize overhead across many items.
5. **Leverage zero-cost abstractions** — iterators, monomorphization, and inline hints.

---

## 2) Mandatory Phase 1: Symptom-Driven Diagnostics (Do This First)

### 1.1 Establish a Reproducible Benchmark Harness
Create a deterministic benchmark using `criterion` or built-in `#[bench]`:
- Fixed input data set
- Warm-up run (discard)
- Steady-state run (10-60s)

Record:
- ops/sec or items/sec
- p50/p95/p99 latency
- CPU utilization (total + per-core)
- RSS / page faults (if available)

**Acceptance:** results reproducible within +/-5-10%.

---

### 1.2 Profiling with perf / flamegraph
Capture a flamegraph under load:

```bash
cargo build --release
perf record -g ./target/release/syauth
perf script | stackcollapse-perf.pl | flamegraph.pl > flame.svg
```

Or use `cargo flamegraph`:
```bash
cargo install flamegraph
cargo flamegraph --bin syauth
```

**What to look for:**
- Wide bars in allocator functions (`alloc`, `dealloc`, `realloc`)
- Lock contention (`pthread_mutex_lock`, `parking_lot`)
- Syscall overhead (`read`, `write`, `futex`)
- Unexpected memcpy/memmove

---

### 1.3 Memory Profiling
Use `DHAT` (via valgrind) or `heaptrack`:

```bash
valgrind --tool=dhat ./target/release/syauth
# or
heaptrack ./target/release/syauth
```

**Red flags:**
- Many small allocations in hot loops
- Growing RSS without matching workload increase
- Frequent allocation/deallocation cycles (use arena or pool)

---

### 1.4 System-Level Profiling
#### Linux
- `perf stat` for hardware counters (cycles, cache misses, branches)
- `iostat -x 1` / `pidstat -d 1` for disk I/O
- `pidstat -w 1` for context switches
- `vmstat 1` for system-wide overview

#### macOS
- `instruments -t "Time Profiler"` for CPU sampling
- `vm_stat 1` for page faults
- `fs_usage -w <pid>` for filesystem activity

---

### 1.5 Classification: Decide Bottleneck Class

**Class A -- Allocation bound**
- Allocator functions dominate flamegraph; many small allocs in hot path

**Class B -- Lock contention**
- Mutex/RwLock/parking_lot dominate; threads waiting

**Class C -- I/O bound**
- read/write/futex dominate; disk metrics show wait; CPU low

**Class D -- Compute bound**
- CPU at 100%; actual computation dominates flamegraph

**Class E -- Cache/memory bound**
- High cache miss rate in `perf stat`; data layout not cache-friendly

Write a short diagnosis note mapping evidence to class.

---

## 3) Mandatory Phase 2: Apply Architecture Pattern Matching Diagnosis

### 2.1 If Class A (Allocation bound): Pool + Arena + Stack
- Use `bumpalo` or `typed-arena` for batch allocations
- Pre-allocate `Vec` with known capacity
- Use `SmallVec` for small, stack-allocated vectors
- Replace `String` with `&str` or `Cow<str>` in hot paths
- Use `bytes::Bytes` for zero-copy buffer sharing

### 2.2 If Class B (Lock contention): Shard + Lock-free
- Replace `Mutex<HashMap>` with `dashmap` or sharded locks
- Use `crossbeam` channels instead of `std::sync::mpsc`
- Use atomic operations for counters and flags
- Per-thread state with thread-local storage

### 2.3 If Class C (I/O bound): Async + Batching
- Use `tokio` or `async-std` for concurrent I/O
- Buffer writes with `BufWriter`
- Batch reads with `BufReader`
- Use `io_uring` via `tokio-uring` for Linux high-perf I/O

### 2.4 If Class D (Compute bound): SIMD + Parallelism
- Use `rayon` for data parallelism
- Consider SIMD via `std::simd` or `packed_simd`
- Profile branch prediction misses and optimize data flow

### 2.5 If Class E (Cache/memory bound): Layout + Prefetch
- Use struct-of-arrays instead of array-of-structs
- Align data to cache lines (64 bytes)
- Group frequently accessed fields together
- Consider `#[repr(C)]` for predictable layout

---

## 4) Common Rust Performance Patterns

### 4.1 Reduce Allocations
```rust
// Pre-allocate with capacity
let mut results = Vec::with_capacity(expected_count);

// Use SmallVec for small collections
use smallvec::SmallVec;
let items: SmallVec<[Item; 8]> = SmallVec::new();

// Reuse buffers
let mut buf = String::with_capacity(1024);
for item in items {
    buf.clear();
    write!(buf, "{}", item).unwrap();
    process(&buf);
}
```

### 4.2 Efficient Concurrency
```rust
// Rayon for data parallelism
use rayon::prelude::*;
let results: Vec<_> = items.par_iter()
    .map(|item| process(item))
    .collect();

// Sharded state
use dashmap::DashMap;
let cache: DashMap<Key, Value> = DashMap::new();
```

### 4.3 Zero-Copy I/O
```rust
// Use bytes for zero-copy
use bytes::{Bytes, BytesMut};
let mut buf = BytesMut::with_capacity(64 * 1024);

// Memory-mapped files for large reads
use memmap2::Mmap;
let file = File::open(path)?;
let mmap = unsafe { Mmap::map(&file)? };
```

---

## 5) Performance Traps

- Optimizing without profiling evidence is premature optimization — it wastes effort and often makes things worse.
- Justify every `.clone()` in hot loops; prefer borrowing or `Cow` when possible.
- Use `&str` or `Cow<str>` instead of `String` in hot paths, because allocation dominates when called millions of times.
- Prefer monomorphization over `Box<dyn Trait>` in hot paths, because dynamic dispatch adds indirection cost.
- Use `write!` to a reused buffer instead of `format!()` in hot paths.
- Set capacity hints on `Vec` to avoid repeated re-allocation from unbounded growth.
- Use sharding or atomics instead of `Mutex` when the protected data supports it.

---

## 6) Deliverables

You must output:
1. Diagnosis summary (evidence → bottleneck class).
2. Specific optimization plan with expected impact.
3. Benchmark before/after comparison (criterion).
4. Concurrency design (if applicable): stages, channels, ownership.
5. Success metrics and how they are measured.

If diagnosis is missing, the design is considered incomplete.

<self_check>

Before proposing any optimization, verify:

- Is the bottleneck class supported by profiling evidence (not intuition)?
- Have you measured a baseline before/after comparison?
- Does the proposed optimization target the actual bottleneck class, not a different one?
- Have you verified that the optimization doesn't introduce correctness regressions?

</self_check>
