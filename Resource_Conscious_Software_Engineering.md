# Resource-Conscious Software Engineering

## A Generic Engineering Philosophy for Preserving CPU, RAM, and Data Movement

> **The cheapest computation is the computation you never perform.**\
> **The cheapest memory is the memory you never allocate.**\
> **The fastest data is the data you never move.**

Resource-efficient software should not begin with bit tricks or
micro-optimizations. It should begin by eliminating unnecessary work,
unnecessary state, unnecessary movement, and unnecessary generality.

The objective is not merely to make existing work faster. The objective
is to make the machine do **less work in the first place**.

------------------------------------------------------------------------

## 1. Eliminate Work Before Optimizing Work

Before optimizing an operation, ask whether the operation needs to
exist.

Prefer:

``` text
remove operation
    ↓
avoid operation
    ↓
reduce frequency
    ↓
simplify operation
    ↓
optimize remaining operation
```

Questions to ask:

1.  Is this result actually needed?
2.  Is it needed now?
3.  Has it already been computed?
4.  Can an earlier stage provide it directly?
5.  Can the problem be reformulated so the operation disappears?
6.  Can several operations be combined into one pass?

A removed operation is faster than any optimized implementation of that
operation.

------------------------------------------------------------------------

## 2. Allocate Only Necessary Memory

Every allocation has potential costs:

1.  allocator bookkeeping,
2.  initialization,
3.  fragmentation,
4.  pointer storage,
5.  cache and TLB pressure,
6.  eventual deallocation,
7.  larger peak memory usage.

Before independently allocating an object, ask:

> **Does this object genuinely need an independent allocation and
> lifetime?**

When many objects share a lifetime, consider:

-   arenas,
-   bump allocation,
-   object pools,
-   contiguous arrays,
-   stack allocation for small bounded data,
-   reusable scratch buffers.

------------------------------------------------------------------------

## 3. Minimize Memory Lifetime

Memory consumption depends on both **size** and **lifetime**.

Do not retain intermediate information after its final consumer has
finished with it.

Prefer:

``` text
produce A
   ↓
consume A
   ↓
discard A
   ↓
reuse memory
```

over retaining every intermediate representation until the end of the
program.

A useful rule is:

> **Release, recycle, or forget data at the earliest provably safe
> point.**

------------------------------------------------------------------------

## 4. Reuse Temporary Memory

Different phases often require temporary storage at different times.

Instead of:

``` text
Phase A → Buffer A
Phase B → Buffer B
Phase C → Buffer C
```

consider:

``` text
Shared scratch memory
      ↑
Phase A
Phase B
Phase C
```

when their lifetimes do not overlap.

This can substantially reduce peak memory without changing the algorithm
itself.

------------------------------------------------------------------------

## 5. Stream Whenever Full Materialization Is Unnecessary

Do not load an entire dataset when only a small portion is required at
one time.

Prefer:

``` text
read → process → emit → forget
```

over:

``` text
read everything
      ↓
store everything
      ↓
process everything
      ↓
emit everything
```

Streaming can make memory consumption depend on the **working set**
rather than total input size.

------------------------------------------------------------------------

## 6. Keep the Working Set Small

Performance is strongly affected by the amount of memory actively
touched during a computation.

Do not attempt to "use all the cache." Instead, design each hot phase so
its working set is small enough to remain as close to the CPU as
practical.

Conceptually:

``` text
Registers
   ↓
L1
   ↓
L2
   ↓
L3
   ↓
RAM
   ↓
Storage
```

The farther data must travel, the more expensive access generally
becomes.

The objective is:

> **Touch as little memory as necessary, as locally as possible.**

------------------------------------------------------------------------

## 7. Prefer Contiguous and Predictable Access

Sequential access is generally easier for caches and hardware
prefetchers than pointer-heavy, irregular traversal.

Where appropriate, prefer:

``` text
array → array → array
```

over:

``` text
pointer → allocation → pointer → allocation → pointer
```

Useful techniques include:

1.  contiguous arrays,
2.  compact vectors,
3.  index-based references,
4.  arenas,
5.  data-oriented layouts,
6.  separating frequently accessed and rarely accessed fields.

------------------------------------------------------------------------

## 8. Separate Hot Data From Cold Data

Frequently accessed information should not necessarily live beside
rarely used metadata.

For example:

``` text
HOT
----
position
size
type
state
ID

COLD
----
debug information
long strings
diagnostics
source metadata
rare options
```

Keeping hot structures small increases the amount of useful information
that can fit in cache lines.

------------------------------------------------------------------------

## 9. Reduce Data Movement

CPU arithmetic is often cheap compared with unnecessary movement through
memory.

Avoid needless:

1.  copies,
2.  temporary buffers,
3.  conversions,
4.  serialization/deserialization,
5.  repeated parsing,
6.  repeated traversal,
7.  moving large structures by value,
8.  reconstruction of equivalent data.

A useful optimization question is not only:

> "How many instructions are executed?"

but also:

> **"How many bytes must move for one useful result?"**

------------------------------------------------------------------------

## 10. Use the Smallest Appropriate Representation

Do not automatically use the widest or most general representation.

Consider the actual range and semantics of:

-   integers,
-   identifiers,
-   indexes,
-   flags,
-   coordinates,
-   enumerations,
-   lengths,
-   counters.

Possible techniques include:

1.  smaller integer types,
2.  compact enums,
3.  bit fields where justified,
4.  packed flags,
5.  indexes instead of full pointers,
6.  fixed-size structures,
7.  normalized internal representations.

But compactness should not create excessive decoding work or obscure
correctness.

------------------------------------------------------------------------

## 11. Prefer Index-Based Structures When Appropriate

Pointer-rich structures can cause:

-   extra allocations,
-   larger representations,
-   unpredictable memory access,
-   fragmentation,
-   pointer chasing.

An alternative is:

``` text
NodeId = integer index
nodes[NodeId]
```

This can allow many related objects to live in one contiguous arena.

Use it when the ownership/lifetime model fits the problem; do not force
it onto naturally independent objects.

------------------------------------------------------------------------

## 12. Precompute Stable Knowledge

If something is expensive to derive but rarely changes, compute it once
rather than repeatedly.

Candidates include:

1.  lookup tables,
2.  classifications,
3.  indexes,
4.  constant mappings,
5.  generated metadata,
6.  parsed static resources,
7.  common transformation tables.

Conceptually:

``` text
expensive work × every execution
```

becomes:

``` text
expensive work × once
+
cheap lookup × every execution
```

The table itself must still justify its memory/cache cost.

------------------------------------------------------------------------

## 13. Compute Lazily

The opposite situation also exists: some information may never be
requested.

Do not eagerly calculate everything simply because it *might* be useful.

Prefer:

``` text
request
   ↓
is result needed?
   ↓ yes
compute
```

Lazy computation is particularly valuable for:

-   expensive derived properties,
-   rarely used metadata,
-   optional features,
-   uncommon error paths,
-   large secondary indexes.

------------------------------------------------------------------------

## 14. Cache Only When Reuse Justifies Memory

Caching is not automatically an optimization.

A cache consumes:

1.  memory,
2.  lookup work,
3.  invalidation logic,
4.  synchronization in some systems,
5.  cache hierarchy capacity.

Cache a result when:

``` text
cost of recomputation × expected reuse
```

is meaningfully greater than:

``` text
storage + lookup + invalidation cost
```

Unbounded caching should generally be avoided.

------------------------------------------------------------------------

## 15. Make Computation Incremental

A small input change should not automatically cause complete
recomputation.

Prefer:

``` text
change
   ↓
find affected dependencies
   ↓
invalidate affected results
   ↓
recompute only those results
```

This requires explicit dependency boundaries, but can dramatically
reduce repeated work in interactive and long-running systems.

------------------------------------------------------------------------

## 16. Specialize the Common Case

Do not force the overwhelmingly common case through machinery required
only for rare complexity.

A useful architecture is:

``` text
common case ──→ short fast path

unusual case ─→ general path
```

Examples include fast paths for:

-   small inputs,
-   common encodings,
-   simple states,
-   default options,
-   ordinary object types.

However, too many fast paths can increase code size and
instruction-cache pressure. Specialization should be measured.

------------------------------------------------------------------------

## 17. Use Phase-Specific Representations

One giant universal data structure is rarely optimal for every stage.

Prefer:

``` text
Input
  ↓
Parsing representation
  ↓
Semantic representation
  ↓
Computation representation
  ↓
Output representation
```

At each transition:

1.  preserve information needed downstream,
2.  discard information that is no longer useful,
3.  choose a representation suited to the next operation.

This can improve both clarity and resource usage.

------------------------------------------------------------------------

## 18. Exploit Established Invariants

Do not repeatedly prove facts already guaranteed by an earlier stage.

If stage A guarantees:

``` text
length > 0
```

then downstream stages should not repeatedly perform the same validation
unless the invariant can be invalidated.

Strong invariants reduce:

-   branches,
-   defensive duplication,
-   repeated validation,
-   state complexity.

The invariant should be explicit and testable.

------------------------------------------------------------------------

## 19. Prefer Single-Pass Algorithms Where They Fit

Multiple full traversals increase CPU work and memory traffic.

If operations can safely be combined:

``` text
Pass 1: inspect
Pass 2: transform
Pass 3: summarize
```

may become:

``` text
Pass 1: inspect + transform + summarize
```

But do not merge passes when doing so makes the algorithm substantially
more complex or destroys locality elsewhere.

------------------------------------------------------------------------

## 20. Choose Algorithms Before Micro-Optimizing Instructions

Algorithmic complexity dominates sufficiently large inputs.

Replacing:

``` text
O(n²)
```

with:

``` text
O(n log n)
```

or:

``` text
O(n)
```

usually matters far more than making the original inner loop slightly
faster.

Optimization priority should generally be:

1.  eliminate unnecessary work,
2.  choose the right algorithm,
3.  choose the right representation,
4.  improve locality,
5.  reduce allocation/data movement,
6.  then optimize instructions.

------------------------------------------------------------------------

## 21. Use Fixed-Point or Integer Arithmetic When It Actually Helps

Fixed-point and integer arithmetic can provide:

-   deterministic behavior,
-   exact representation for selected domains,
-   compact storage in some cases,
-   predictable semantics.

But modern processors often execute floating-point operations very
efficiently.

Therefore:

> **Choose numeric representation for correctness, determinism, range,
> precision, memory, and measured performance---not nostalgia.**

------------------------------------------------------------------------

## 22. Replace Repeated Logic With Tables When Beneficial

Small lookup tables can replace:

-   complicated classification logic,
-   repeated branching,
-   repeated transformations,
-   expensive calculations.

But a huge table can create more memory traffic than the computation it
replaces.

Use tables when they are:

1.  compact,
2.  frequently reused,
3.  cache-friendly,
4.  cheaper than recomputation.

------------------------------------------------------------------------

## 23. Bound Resource Growth

Important data structures should have understood growth behavior.

Watch particularly for:

-   queues,
-   caches,
-   recursion,
-   temporary buffers,
-   hash tables,
-   retry loops,
-   work lists,
-   generated intermediate data.

Ask:

1.  What determines maximum size?
2.  What happens for pathological input?
3.  Can memory grow without bound?
4.  Is there a graceful fallback?
5.  Can work be chunked?

Predictability is itself a performance feature.

------------------------------------------------------------------------

## 24. Avoid Generality That Is Not Needed

Generality often costs:

-   metadata,
-   indirection,
-   branching,
-   larger structures,
-   more complex ownership,
-   more code.

Do not design every component for hypothetical requirements.

Prefer the simplest representation that correctly supports the actual
problem, while leaving clean boundaries for future extension.

------------------------------------------------------------------------

## 25. Avoid Abstraction in Hot Paths When It Has Measurable Cost

Abstraction is valuable, but abstractions can sometimes introduce:

-   dynamic dispatch,
-   allocations,
-   hidden copies,
-   unpredictable branches,
-   unnecessary conversions.

Keep high-level interfaces where they improve correctness and
maintainability. In genuinely hot paths, inspect what those abstractions
compile into.

The rule is not "avoid abstraction."

It is:

> **Do not pay repeatedly for abstraction that provides no value in the
> hot path.**

------------------------------------------------------------------------

## 26. Keep Cold Error Paths Cold

Normal execution should not carry unnecessary costs for rare failures.

Diagnostics, detailed formatting, stack construction, and expensive
debugging information can often be deferred until an error actually
occurs.

Fast success paths and rich diagnostics are compatible when the
architecture separates them properly.

------------------------------------------------------------------------

## 27. Minimize Startup Work

Short-lived programs can spend a large fraction of their total time
initializing.

Avoid unnecessary startup:

1.  large runtime table construction,
2.  eagerly loading optional resources,
3.  initializing unused subsystems,
4.  scanning directories unnecessarily,
5.  building indexes before they are requested.

Precompute or lazily initialize when appropriate.

------------------------------------------------------------------------

## 28. Reuse Results Across Boundaries Carefully

If the same expensive information is needed by multiple stages, consider
computing it once and passing the result forward.

Avoid:

``` text
Stage A computes X
Stage B discards X
Stage C recomputes X
```

when:

``` text
Stage A computes X
        ↓
Stage C receives X
```

is cheap and does not create excessive memory lifetime.

This is a trade-off between recomputation and retention; measure both.

------------------------------------------------------------------------

## 29. Design for Locality, Not Merely Small Size

A 16-byte structure is not automatically efficient if accessing it
requires random pointer chasing.

Likewise, a somewhat larger contiguous structure can outperform a
smaller scattered structure.

Resource engineering should consider together:

-   representation size,
-   access order,
-   spatial locality,
-   temporal locality,
-   number of indirections,
-   working-set size.

------------------------------------------------------------------------

## 30. Avoid Branches That Carry Little Information

Unpredictable branches can stall modern pipelines.

When profiling identifies branch-heavy hot loops, possibilities include:

-   table-driven classification,
-   grouping similar cases,
-   separating common and uncommon paths,
-   processing homogeneous batches.

Do not make code branchless merely for appearance. A predictable branch
may be cheaper than extra arithmetic or table access.

------------------------------------------------------------------------

## 31. Batch Work When Setup Cost Is Significant

If an operation has a fixed setup cost, performing many tiny independent
operations may waste resources.

Where latency requirements permit:

``` text
item → setup → process
item → setup → process
item → setup → process
```

can become:

``` text
batch → setup once → process many
```

Batching can improve:

-   locality,
-   amortization,
-   system-call efficiency,
-   vectorization opportunities.

------------------------------------------------------------------------

## 32. Minimize Expensive Boundary Crossings

Crossings such as these may be disproportionately expensive:

-   system calls,
-   process boundaries,
-   network requests,
-   disk access,
-   runtime/FFI boundaries,
-   synchronization primitives.

Do useful work per crossing, but avoid giant batches that damage latency
or memory use.

------------------------------------------------------------------------

## 33. Keep Synchronization Out of Uncontended Work When Possible

Concurrency can increase throughput, but it also introduces:

-   locks,
-   atomics,
-   scheduling,
-   cache-line contention,
-   synchronization,
-   larger working sets.

Do not parallelize work merely because multiple cores exist.

Parallelism is beneficial when:

``` text
useful parallel work
>
coordination + communication + contention
```

A highly efficient single-threaded core can sometimes outperform a
poorly partitioned parallel design.

------------------------------------------------------------------------

## 34. Reduce False Sharing

When threads modify unrelated values located on the same cache line,
they can still cause cache-coherence traffic.

For heavily written concurrent state:

1.  identify ownership,
2.  partition writable data,
3.  avoid unnecessary shared counters,
4.  pad/separate hot independently modified values when measurements
    justify it.

This is a locality problem between cores rather than within one core.

------------------------------------------------------------------------

## 35. Make Resource Ownership Explicit

Resource-conscious systems benefit from clear answers to:

-   Who owns this memory?
-   Who may modify it?
-   How long must it live?
-   Who releases or reuses it?
-   Can it be borrowed instead of copied?

Clear ownership often exposes unnecessary allocations and copies
naturally.

------------------------------------------------------------------------

## 36. Treat I/O as a Scarce Resource

Avoid unnecessary:

-   file opens,
-   seeks,
-   reads,
-   writes,
-   flushes,
-   network round trips.

Buffer appropriately, batch small operations when useful, and avoid
writing information that can be generated cheaply when needed.

For many workloads, I/O dominates CPU optimization.

------------------------------------------------------------------------

## 37. Compress Only When the Trade-Off Makes Sense

Compression exchanges CPU for:

-   reduced storage,
-   reduced I/O,
-   reduced network transfer,
-   sometimes improved effective cache capacity.

Compression is beneficial when saved movement/storage costs exceed
compression/decompression costs.

Do not compress data merely because it can be compressed.

------------------------------------------------------------------------

## 38. Prefer Deterministic Resource Behavior

Predictable software is easier to optimize than software whose work
varies unpredictably.

Useful properties include:

-   deterministic algorithms,
-   bounded caches,
-   explicit memory lifetimes,
-   stable traversal order,
-   controlled recursion,
-   documented fallbacks.

Determinism also makes performance regressions easier to reproduce.

------------------------------------------------------------------------

## 39. Measure Useful Work, Not Only Wall Time

Wall-clock time tells you whether something became faster. It often does
not tell you why.

Useful metrics include:

1.  CPU cycles per useful operation,
2.  instructions per useful operation,
3.  allocations per useful operation,
4.  bytes allocated per useful operation,
5.  peak resident memory,
6.  working-set size,
7.  bytes copied,
8.  L1 data-cache misses,
9.  L2 misses where measurable,
10. last-level-cache misses,
11. branch misses,
12. page faults,
13. system calls,
14. I/O bytes,
15. startup time,
16. output size,
17. energy consumption where measurable.

Normalize measurements by meaningful work whenever possible.

------------------------------------------------------------------------

## 40. Establish Resource Budgets

Instead of merely saying "use little memory," establish measurable
budgets.

Examples:

``` text
maximum peak memory per unit of input
maximum allocations per operation
maximum bytes copied per request
maximum startup time
maximum cache-miss rate
maximum temporary workspace
```

Budgets make resource efficiency an engineering constraint rather than
an aspiration.

------------------------------------------------------------------------

## 41. Optimize Representative Workloads

An optimization that wins one microbenchmark can lose on real workloads.

Maintain several workload classes:

1.  tiny inputs,
2.  typical inputs,
3.  large inputs,
4.  pathological inputs,
5.  cold startup,
6.  warm repeated execution,
7.  memory-constrained execution,
8.  concurrent execution where relevant.

Optimization decisions should consider the complete workload
distribution.

------------------------------------------------------------------------

## 42. Make Performance Regressions Testable

Resource usage should be treated like correctness.

Automated checks can detect regressions in:

-   runtime,
-   allocation count,
-   peak memory,
-   binary size,
-   startup cost,
-   throughput.

Not every metric needs a hard CI threshold, but important budgets should
be observable continuously.

------------------------------------------------------------------------

## 43. Do Not Trade Unlimited Complexity for Small Savings

Optimization itself consumes resources:

-   engineering time,
-   code size,
-   review effort,
-   debugging effort,
-   maintenance effort,
-   cognitive load.

A technically faster solution may be worse overall if it creates
fragile, incomprehensible code for negligible savings.

Ask:

> **Is the resource saved worth the complexity introduced?**

------------------------------------------------------------------------

## 44. Preserve Escape Hatches

The common case can be highly optimized while uncommon cases use a
slower general path.

This often provides a better architecture than forcing the hot
representation to support every possible feature.

``` text
common case → compact optimized path
                     |
                     └── exceptional case → general fallback
```

This principle allows efficiency without artificially limiting
capability.

------------------------------------------------------------------------

## 45. Recommended Optimization Order

When improving an existing system, use roughly this order:

1.  **Measure the real workload.**
2.  **Remove unnecessary work.**
3.  **Remove unnecessary data.**
4.  **Choose better algorithms.**
5.  **Reduce memory lifetime.**
6.  **Reduce allocations.**
7.  **Reduce copying and data movement.**
8.  **Improve data locality.**
9.  **Reduce the working set.**
10. **Precompute stable expensive information.**
11. **Make optional work lazy.**
12. **Add incremental computation where reuse exists.**
13. **Specialize important common cases.**
14. **Reuse temporary storage.**
15. **Compact representations where beneficial.**
16. **Optimize branches and individual instructions only after
    profiling.**
17. **Re-measure the complete workload.**

This ordering prevents spending days optimizing an operation that should
simply have been eliminated.

------------------------------------------------------------------------

## 46. The Core Questions

For every hot operation or important data structure, ask:

1.  **Does this need to exist?**
2.  **Does this need to happen?**
3.  **Does it need to happen now?**
4.  **Does it need to happen this often?**
5.  **Does all of this data need to be stored?**
6.  **Does it need to remain stored this long?**
7.  **Does it need an independent allocation?**
8.  **Does it need to be copied?**
9.  **Can it be streamed?**
10. **Can it be computed once?**
11. **Can it be computed lazily?**
12. **Can a smaller representation express it?**
13. **Can related data be made contiguous?**
14. **Can hot and cold data be separated?**
15. **Can the common case avoid the general path?**
16. **Can a small change avoid global recomputation?**
17. **Is the algorithm bounded under pathological input?**
18. **Does the optimization improve measured useful work?**

These questions capture the philosophy more reliably than any individual
optimization technique.

------------------------------------------------------------------------

## 47. Final Philosophy

Resource-conscious engineering is not primarily about making every
instruction clever.

It is about progressively removing waste:

``` text
Do less.
Store less.
Move less.
Retain less.
Recompute less.
Generalize less.
Touch less memory.
Cross fewer boundaries.
```

Then, after the unnecessary work has disappeared:

``` text
make the remaining work
simple,
local,
predictable,
compact,
and measurable.
```

The final principle is:

> **Eliminate what is unnecessary. Simplify what remains. Keep data
> close. Reuse what is expensive. Measure the result. Optimize only what
> still matters.**
