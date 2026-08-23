# cljrsh

cljrsh (Clojure Rust Shell) is a babashka-style scripting binary built on
clojurust. It starts in about 15 ms, needs no JVM, and ships as one
self-contained executable: the Clojure runtime, a scripting standard library,
an HTTP client, JSON/YAML/CSV codecs, a Kubernetes client, an AWS client, and
an embedded nushell engine.

If you know babashka, you already know cljrsh: the CLI grammar, `bb.edn`
tasks, pods, preloads, and exit-code conventions all match. Scripts written
for babashka's `babashka.*` namespaces run against the built-in compatibility
layer; new code should prefer the first-party `cljrsh.*` namespaces.

## Install

Build from this repository with cargo:

```bash
cargo install --git https://github.com/andrewdean/clojurust cljrsh
```

For the smaller distribution build (thin LTO, stripped, ~60 MB instead of
~94 MB), install from a checkout:

```bash
git clone https://github.com/andrewdean/clojurust
cd clojurust
cargo install --path crates/cljrsh --profile dist
```

Three cargo features are on by default and can be disabled for faster builds
or a smaller binary: `nu` (embedded nushell), `aws` (AWS client), and `k8s`
(Kubernetes client). For example, `--no-default-features` builds the core
shell alone.

New here? The [tutorial series](../../docs/tutorials/README.md) walks from
install through the REPL, scripting, Kubernetes, AWS, Terraform, schemas,
and configuration, with every example verified against the tree.

## Quick start

Evaluate an expression, run a script, or pipe a program through stdin:

```bash
cljrsh -e '(+ 1 2 3)'
cljrsh script.clj arg1 arg2      # args bound to *command-line-args*
echo '(println (slurp "x.txt"))' | cljrsh
```

Scripts work as shebang executables, with line numbers preserved in errors:

```clojure
#!/usr/bin/env cljrsh
(require '[cljrsh.fs :as fs])
(doseq [f (fs/glob "." "**/*.md")]
  (println (str f)))
```

Stream processing mirrors babashka's `-i` / `-o` flags. The following prints
every line of input that contains "error":

```bash
journalctl -u myapp | cljrsh -io -e '(filter #(re-find #"error" %) *input*)'
```

## Tasks

cljrsh reads the same `bb.edn` files babashka does: `:paths`, `:deps` (git
deps), and `:tasks` with `:depends` ordering.

```bash
cljrsh tasks         # list tasks with docstrings
cljrsh run build     # run a task (and its :depends closure)
cljrsh prepare       # fetch :deps ahead of time
```

A bare `cljrsh <name>` also runs the task `<name>` when no file of that name
exists, so task invocation reads like a subcommand.

## REPL

`cljrsh` with no program on a terminal starts a rustyline REPL. The prompt
shows the current namespace, history persists in `~/.cache/cljrsh/history`,
and Tab completes special forms, vars in scope, namespace aliases, and
`alias/`-qualified publics. Ctrl-C interrupts the running form (a second
Ctrl-C force-quits); `finally` blocks still run during the unwind.

`clojure.repl` works as in Clojure: `doc`, `source`, `apropos`, `dir`,
`find-doc`, and `pst`. `source` shows the defining form of any var loaded
from a source file, including git deps.

Two server modes serve editors and remote sessions:

```bash
cljrsh nrepl-server        # bencode nREPL for CIDER/Calva/Conjure (port 1667)
cljrsh socket-repl         # plain text REPL over TCP (port 1666)
```

## Built-in namespaces

First-party namespaces live under `cljrsh.*`:

| Namespace | Provides |
|-----------|----------|
| `cljrsh.fs` | file-system operations (glob, walk, copy-tree, temp files, symlinks, permissions) |
| `cljrsh.process` | subprocess spawning with redirection and pipelines |
| `cljrsh.http` | HTTP/HTTPS client (get, post, streaming, timeouts) |
| `cljrsh.json`, `cljrsh.yaml`, `cljrsh.csv` | data codecs |
| `cljrsh.io` | stream and reader/writer utilities |
| `cljrsh.term` | terminal size, colors, raw mode |
| `cljrsh.wait` | wait-for-port / wait-for-path polling |
| `cljrsh.config` | layered EDN/env configuration loading |
| `cljrsh.hash` | md5/sha digests |
| `cljrsh.datalog` | in-memory datalog queries |

The babashka compatibility layer (`babashka.fs`, `babashka.process`,
`babashka.http-client`, `babashka.cli`, `babashka.wait`, `babashka.terminal`)
and the Clojure contrib surface (`clojure.java.io`, `clojure.java.shell`,
`clojure.data.csv`, `clojure.pprint`, `clojure.repl`, `clojure.spec.alpha`,
`clojure.math`, and the rest of the embedded stdlib) load on first `require`.

Feature-gated namespaces:

| Namespace | Feature | Provides |
|-----------|---------|----------|
| `nu` | `nu` | embedded nushell: `(nu/eval "ls \| where size > 1kb")` returns Clojure data |
| `k8s` | `k8s` | data-driven Kubernetes client (any resource, CRDs included, as maps) |
| `aws` | `aws` | SigV4 AWS client: S3 (Garage-compatible) plus generic awsJson invoke |

Pods complete the picture: `(pods/load-pod ...)` speaks the babashka pod
protocol, so existing pods work unchanged.

## Exit codes and conventions

`(System/exit N)` and a thrown `ex-info` carrying `:cljrsh/exit N` (or
`:babashka/exit N`) exit with code N; the ex-info form prints only its
message. Uncaught errors report and exit 1, usage errors exit 2, SIGPIPE
exits 141, and an interrupted script exits 130. Reader conditionals see the
features `#{:bb :cljrsh :rust}`, so `#?(:bb ...)` code paths written for
babashka are taken here too. `CLJRSH_PRELOADS` (or `BABASHKA_PRELOADS`)
evaluates before the program.

## File layout

- `src/main.rs` — entry: SIGPIPE reset, SIGINT handler (interrupt flag +
  nushell signal), arg dispatch, big-stack interpreter thread, Tokio
  current-thread runtime + LocalSet, GC mutator registration.
- `src/opts.rs` — hand-rolled file-first argument grammar (`Program`,
  `Opts`, `parse`, `usage`).
- `src/exec.rs` — `setup_globals` (stdlib + host namespaces + reader
  features), `run_program` (shebang, preloads, exit-code mapping),
  `eval_str`/`eval_form`.
- `src/repl.rs` — rustyline REPL with tab completion and ns-aware prompt,
  delimiter-balance multi-line accumulation, history; socket REPL.
- `src/tasks.rs` — bb.edn task runner (`tasks`/`run`/bare-name dispatch).
- `tests/cli.rs` — end-to-end binary tests.

The library surface lives in sibling crates: `cljrsh-host` (the `cljrsh.*` /
`babashka.*` namespaces), `cljrsh-project` (bb.edn parsing and the task
graph), `cljrsh-pods`, `cljrsh-nu`, `cljrsh-aws`, and `cljrsh-k8s`.
