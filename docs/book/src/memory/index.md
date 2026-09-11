# Memory Management

Every Clojure value in clojurust lives behind a `GcPtr<T>` — a raw pointer
into managed memory. clojurust uses **two** allocators that cooperate behind
that pointer:

| Allocator | When it runs | Reclaims memory |
|---|---|---|
| **Tracing GC** (mark-and-sweep) | always, in the default build | during `collect` (stop-the-world) |
| **Bump allocator** (regions) | AOT-compiled code only | in bulk, when a region's scope ends |

The garbage collector is the backstop: it manages the full object graph and is
the only allocator the interpreter uses. The bump allocator is a **compile-time
optimization** layered on top — when the AOT compiler can prove that an object
does not outlive the function (or loop) that created it, it allocates that
object in a region that is freed all at once, with no tracing and no GC pause.

You don't manage either allocator by hand. You write ordinary Clojure; the
compiler decides what is region-eligible and the GC handles everything else.

## Tracing GC

The default collector is a **non-moving, stop-the-world mark-and-sweep** GC.
Key properties:

- `GcPtr<T>` stores a stable address — objects never move, so a pointer stays
  valid for the lifetime of the object.
- `clone` is an O(1) pointer copy; `drop` is a no-op. Reference **cycles are
  collected**, because liveness is determined by reachability from roots, not
  reference counts.
- Collection is triggered by a memory threshold. The default hard limit is
  1/4 of system RAM (a fixed **64 MB** soft limit on `wasm32`, which cannot
  query system memory).
- Each OS thread (isolate) owns an independent heap and collects on its own,
  with no cross-thread coordination.

## Bump allocator (regions)

The [bump allocator](bump-allocator.md) is a region-based fast path for
short-lived, non-escaping allocations. It is selected automatically by the
AOT compiler's **escape analysis** and is described in detail in the next
chapter.

> **AOT only.** The bump allocator currently runs only in AOT-compiled
> binaries (`cljrs compile`). The interpreter (`cljrs run`, `cljrs repl`,
> `cljrs eval`) allocates everything on the GC heap. See
> [The bump allocator](bump-allocator.md) for why.

## Inspecting allocation behaviour

Both allocators feed a single set of process-global counters. Set the
`CLJRS_GC_STATS` environment variable to dump them at program exit:

```bash
CLJRS_GC_STATS=- ./myapp        # write a summary to stdout
CLJRS_GC_STATS=stats.txt ./myapp # write the summary to a file
```

The summary reports GC allocations and bytes, **region (bump) allocations and
bytes**, GC collection count, total pause time, and objects/bytes freed — so
you can see how much work the bump allocator is taking off the GC. It also
reports isolate-boundary crossings (count, bytes copied, the largest single
crossing, and a `<=4 KiB` / `<=256 KiB` / `>256 KiB` size histogram) and the
process memory-pressure line described below. The interpreter exposes the
same counters through the `cljrs --gc-stats [FILE]` flag.

## Process memory pressure

Every isolate owns its own heap and collects on its own schedule, so no single
heap can see that the *process* is close to its memory budget. A process-wide
coordinator sums the live bytes of every heap and derives a graduated level:

| Level | Enters at | Leaves below | Response |
|---|---|---|---|
| `:green` | — | — | none |
| `:yellow` | 75 % of budget | 70 % | every heap collects at half its soft limit |
| `:red` | 90 % of budget | 85 % | heaps collect at a quarter of their soft limit; network accept loops stop accepting until the level drops |

The budget defaults to the same RAM-derived value as a single heap's hard
limit (a quarter of RAM, capped at 4 GiB). Set `CLJRS_GC_PROCESS_LIMIT_MB` to
override it, or to `0` to turn the coordinator off. Heaps report in 1 MiB
steps, so the hot allocation path touches the shared counter once per
megabyte rather than per object.

From Clojure, `(clojure.core.async/memory-pressure)` returns the current
level as a keyword and `(await (memory-pressure-below :red))` parks an async
body until load shedding ends — the hook for a server loop that wants to stop
taking from `:conns` itself. Transitions are counted on the
`Memory pressure:` line of the GC stats summary and logged under the `gc`
debug feature.
