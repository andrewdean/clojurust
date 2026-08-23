# cljrs-tx

## Purpose

Runs pure Datomic-style transaction functions in a fresh, GC-less,
tree-walker-only invocation arena.

## Status

Initial transaction-runtime implementation. IR lowering, JIT, AOT, async,
I/O, clocks, randomness, process-global mutable values, and Rust interop are
outside this crate's execution profile.

Build with the crate's `no-gc` feature (`cargo test -p cljrs-tx --features
no-gc`). The feature is not enabled by default so workspace-wide GC builds do
not accidentally unify every clojurust dependency into `no-gc` mode.

The default `host-dependencies` feature preserves Clojurust's dependency and
Git resolution behavior. Restricted embeddings can exclude those subsystems
from the compiled graph:

```console
cargo test -p cljrs-tx --no-default-features --features no-gc
```

The restricted transaction profile still enforces the runtime transaction
policy. Versioned lookup attempts return a deterministic forbidden-effect
error, and the compiled graph contains no `cljrs-deps`, `cljrs-vcs`, `gix`,
`pgp`, or `ssh-key` package.

## File layout

- `src/lib.rs` — parsed program, limits, isolated execution boundary, pure-data validation, and tests.

## Public API

```rust
pub struct TxProgram { /* parsed immutable form */ }
impl TxProgram {
    pub fn parse(source: &str) -> Result<Self, TxError>;
    pub fn form(&self) -> &cljrs_reader::Form;
}

pub struct TxLimits {
    pub memory_bytes: usize,
    pub gas: u64,
    pub call_depth: u64,
}

pub enum TxError { /* read, boundary, evaluation, budget, and policy errors */ }

pub trait HostFn: Send + Sync {
    fn call(&self, args: &[SerializedValue]) -> Result<SerializedValue, String>;
}
// Blanket impl for Fn(&[SerializedValue]) -> Result<SerializedValue, String> + Send + Sync.

pub struct HostApi { /* namespaced host functions */ }
impl HostApi {
    pub fn new() -> Self;
    pub fn define(&mut self, namespace: impl Into<Arc<str>>, name: impl Into<Arc<str>>, function: impl HostFn + 'static);
    pub fn is_empty(&self) -> bool;
}

pub fn execute(
    program: &TxProgram,
    args: Vec<cljrs_value::clone::SerializedValue>,
    limits: TxLimits,
) -> Result<cljrs_value::clone::SerializedValue, TxError>;

pub fn execute_with_host(
    program: &TxProgram,
    args: Vec<cljrs_value::clone::SerializedValue>,
    limits: TxLimits,
    host: &HostApi,
) -> Result<cljrs_value::clone::SerializedValue, TxError>;
```

`execute` creates and bootstraps a new interpreter environment inside one
bounded arena, structured-clones arguments in, invokes the function under a
gas meter and transaction capability policy, clones a pure-data result out,
and then destroys the whole environment.

`execute_with_host` additionally interns each `HostApi` entry as a namespaced
native function (e.g. `my.host/lookup`) in the invocation environment. Host
calls are the only seam through the isolation boundary: arguments are
serialized out of the arena and validated as pure data, the result is
validated and structured-cloned back in (arena allocation, so the invocation's
managed-memory budget covers it). Host functions should be deterministic,
side-effect-free reads; a returned `Err` surfaces inside the transaction as an
ordinary evaluation error. This is the hook an embedding database uses to
expose read-only query APIs to transaction functions without letting live
handles enter the arena.

`call_depth` caps nested interpreted function applications through the
function-application hook. Each interpreted frame consumes real Rust stack
and an overflow is process-fatal, so hosts must size the executing thread's
stack to comfortably cover the configured depth (interpreted frames can run
to a few kilobytes each).

The memory limit covers arena object boxes and out-of-line memory reported by
`Trace::gc_size_extra`. It is a managed-allocation limit, not yet a hard RSS
limit: ordinary Rust allocations outside traced values are not fully charged.
A WASM linear-memory or process sandbox remains necessary when an adversarial
hard address-space ceiling is required.
