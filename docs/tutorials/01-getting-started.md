# Getting started

This tutorial installs the two clojurust binaries and runs a first program
with each. By the end you will know which binary to reach for and why.

## Two binaries, one runtime

The workspace builds two executables that share the same Clojure runtime:

| Binary | Role | Reach for it when |
|--------|------|-------------------|
| `cljrs` | Language runtime: tree-walking interpreter, IR interpreter, Cranelift JIT, AOT compiler, LSP, IR tooling | You are developing long-running programs, compiling standalone binaries, or working on the language itself |
| `cljrsh` | Scripting shell: file-first CLI, bb.edn tasks, host libraries (fs, http, json, k8s, aws, ...), babashka compatibility | You are writing scripts, automation, and operational tooling |

In other words: `cljrs` is the compiler toolchain, `cljrsh` is the daily
driver. The rest of this series spends most of its time in `cljrsh`.

## Install

Both install from the repository with cargo:

```bash
cargo install --git https://github.com/andrewdean/clojurust cljrs
cargo install --git https://github.com/andrewdean/clojurust cljrsh
```

For the smaller distribution build of cljrsh (thin LTO, stripped: ~60 MB
instead of ~94 MB), install from a checkout:

```bash
git clone https://github.com/andrewdean/clojurust
cd clojurust
cargo install --path crates/cljrsh --profile dist
```

Verify the install:

```bash
$ cljrsh -e '(+ 1 2 3)'
6
```

That invocation took about 15 ms end to end; there is no JVM and no warmup.

## First programs

Run an expression, a file, or stdin. Trailing arguments land in
`*command-line-args*`:

```bash
$ cljrsh -e '(println "hello," (first *command-line-args*))' world
hello, world

$ echo '(println (count (slurp "/etc/hostname")))' | cljrsh
9
```

The same program runs under `cljrs` with JIT acceleration:

```bash
$ cljrs eval '(reduce + (range 1000000))'
499999500000
```

For compute-heavy code the difference matters: `cljrs run` tiers hot
functions from the tree-walker to an IR interpreter and then to native code
via Cranelift while the program runs. `cljrsh` favors startup latency and
batteries instead. Both read the same `.clj`, `.cljc`, and `.cljrs` sources.

## Compile to a native binary

`cljrs` also compiles ahead of time:

```bash
$ cat hello.cljrs
(defn -main [& args]
  (println "compiled hello to" (first args)))

$ cljrs compile hello.cljrs -o hello
$ ./hello world
compiled hello to world
```

The AOT path reuses the JIT's Cranelift backend, so what runs hot under
`cljrs run` compiles the same way in the standalone binary.

## Reader conditionals

Sources are portable across the Clojure family through reader conditionals.
The platform key is `:rust`, and `cljrsh` additionally answers to `:bb` and
`:cljrsh` so babashka scripts take their babashka branches:

```clojure
#?(:bb    (println "in babashka or cljrsh")
   :clj   (println "on the JVM")
   :rust  (println "in cljrs"))
```

`cljrsh`'s feature set is `#{:bb :cljrsh :clj :rust}`; ecosystem `.cljc`
libraries that branch on `:clj` load their JVM branches, which the runtime's
interop shims (`clojure.java.io`, `StringBuilder`, `Long/parseLong`, and
friends) are there to satisfy.

## Where to next

Start the REPL with a bare `cljrsh` and continue to
[the REPL tutorial](02-repl.md), or jump straight to
[scripting](03-scripting.md) if you came from babashka and want your
workflows back.
