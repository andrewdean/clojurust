# The REPL

This tutorial tours the interactive workflow: line editing, completion, the
`clojure.repl` introspection tools, interrupt handling, and connecting an
editor. Start it with a bare `cljrsh` on a terminal.

```
$ cljrsh
cljrsh 0.1.0 — :repl/quit or Ctrl-D to exit
user=>
```

The prompt shows the current namespace and follows you through `in-ns`:

```
user=> (in-ns 'scratch.core)
#<Namespace scratch.core>
scratch.core=>
```

History persists in `~/.cache/cljrsh/history` across sessions. Multi-line
input works by delimiter balance: an unclosed form switches the prompt to
`  ...  ` and keeps reading until the form closes.

## Tab completion

Tab completes from everything in scope: special forms, vars interned or
referred into the current namespace, namespace aliases, and namespace names.
After `(require '[cljrsh.fs :as fs])`:

- `redu<TAB>` completes to `reduce`.
- `fs/create-sy<TAB>` completes to `fs/create-sym-link`; alias-qualified
  completion lists only that namespace's public vars.
- `(require 'clojure.pp<TAB>` completes to `clojure.pprint`; namespace names
  complete even for built-in namespaces that have not loaded yet.

## Introspection with clojure.repl

`clojure.repl` gives you the standard toolkit:

```clojure
user=> (require '[clojure.repl :refer [doc source apropos dir]])
user=> (doc map)
-------------------------
clojure.core/map
[[f] [f coll] [f c1 c2] [f c1 c2 c3] [f c1 c2 c3 & colls]]
  Returns a lazy sequence consisting of the result of applying f to
  ...
```

`apropos` searches var names across all loaded namespaces, by substring or
regex:

```clojure
user=> (apropos "sym-link")
(cljrsh.fs/create-sym-link cljrsh.fs/sym-link?)
```

`dir` prints a namespace's public vars; vars defined with `defn-` stay
hidden:

```clojure
user=> (require 'clojure.pprint)
user=> (dir clojure.pprint)
*print-right-margin*
cl-format
pp
pprint
print-table
write
```

`source` prints the defining form of any var whose namespace loaded from a
source file, dependencies from Clojars included:

```clojure
user=> (require '[medley.core])
user=> (source medley.core/deep-merge)
(defn deep-merge
  "Recursively merges maps together. ..."
  ...)
```

One limitation to know: `source` extracts text from the recorded source
file, so vars defined at the REPL or inside the built-in runtime have no
source to show. See `docs/cljrsh-divergences.md` for details.

## Interrupting evaluation

Ctrl-C interrupts the running form without killing the session; `finally`
blocks still run during the unwind:

```clojure
user=> (loop [] (recur))
^CInterrupted.
user=> (+ 1 2)
3
```

A second Ctrl-C while the first is still unwinding (native code that never
reaches a checkpoint) force-exits the process with code 130. In scripts, the
same interrupt exits 130 in the babashka tradition.

## Server modes

Two servers expose the same evaluator over the network. The nREPL server
speaks bencode for CIDER, Calva, and Conjure:

```bash
$ cljrsh nrepl-server          # 127.0.0.1:1667
```

From Emacs, `cider-connect` to port 1667; completion, doc lookup, load-file,
and interrupt are advertised. The socket REPL is the plain-text alternative,
handy from netcat or another script:

```bash
$ cljrsh socket-repl           # 127.0.0.1:1666
$ nc localhost 1666
user=> (* 6 7)
42
```

For projects, `cljrsh print-deps --format classpath` feeds classpath-aware
tools; clojure-lsp picks up bb.edn projects through a `:project-specs` entry
that shells out to it.

## Where to next

The REPL is where you explore; [scripting](03-scripting.md) is where the
results land.
