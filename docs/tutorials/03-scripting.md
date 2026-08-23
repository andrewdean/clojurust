# Scripting

This tutorial covers the daily scripting workflow: standalone scripts, stdin
pipelines, project tasks, dependencies, pods, and bundling. If you know
babashka, everything here will look familiar; the CLI grammar, `bb.edn`
format, and exit conventions match on purpose.

## Scripts and shebangs

A script is a file of forms; arguments arrive in `*command-line-args*`:

```clojure
#!/usr/bin/env cljrsh
;; wordcount: top three words on stdin
(->> (line-seq *in*)
     (mapcat #(clojure.string/split % #"\s+"))
     (frequencies)
     (sort-by val >)
     (take 3)
     (run! (fn [[w n]] (println n w))))
```

```bash
$ printf 'the cat sat on the mat the end\n' | ./wordcount
3 the
1 cat
1 sat
```

`*in*` is the process stdin; `line-seq` walks it lazily, so the script
streams arbitrarily large input. Errors report with file, line, and a caret
snippet; the shebang line does not shift line numbers.

## Input and output flags

The `-i`/`-o` family binds stdin to `*input*` and prints the result, which
turns one-liners into pipeline stages:

```bash
$ printf 'error: one\nok: two\nerror: three\n' | \
    cljrsh -io -e '(filter #(re-find #"^error" %) *input*)'
error: one
error: three
```

Capital variants switch to EDN: `-I` reads stdin as EDN values and `-O`
prints with `prn`. `--stream` re-evaluates the expression per line (or per
EDN value), for infinite pipelines.

## Calling functions from the CLI

`-x` calls a function with flags parsed by babashka.cli, and `-m` runs a
namespace's `-main`. Given `src/mylib.clj` on the project `:paths`:

```clojure
(ns mylib)
(defn greet [{:keys [name times] :or {times 1}}]
  (dotimes [_ times] (println "Hello," name)))
```

```bash
$ cljrsh -x mylib/greet --name Ada --times 2
Hello, Ada
Hello, Ada
```

Flag strings coerce to numbers and keywords the way babashka.cli defines;
you write a plain function of one map and get a CLI for free.

## Exit codes

Scripts signal outcomes the babashka way. `(System/exit 3)` exits with 3
from anywhere, uncatchably (a `finally` still runs). Throwing
`(ex-info "msg" {:cljrsh/exit 3})` (or `:babashka/exit`) does the same but
prints only the message: the idiom for clean CLI errors. Uncaught errors
report and exit 1, usage errors exit 2, SIGPIPE exits 141, and Ctrl-C exits
130.

## Projects and tasks

A `bb.edn` in the directory (or any parent) defines source paths,
dependencies, and tasks:

```clojure
{:paths ["src"]
 :deps  {dev.weavejester/medley {:mvn/version "1.8.1"}}
 :tasks {:init (def greeting "hello")
         clean {:doc "Remove build artifacts"
                :task (println "cleaning...")}
         build {:doc "Compile the project"
                :depends [clean]
                :task (println greeting "- building...")}}}
```

```bash
$ cljrsh tasks          # list with docstrings
$ cljrsh run build
cleaning...
hello - building...
$ cljrsh build          # bare task name works when no file shadows it
```

`:depends` runs prerequisites in topological order, `:init` evaluates before
any task, and `:enter`/`:leave` hooks wrap task bodies. Inside a task,
`(current-task)` returns its name and doc.

Dependencies resolve without a JVM: `:mvn/version` coordinates download from
Clojars and Maven Central, `:git/url` deps fetch via git, and `:local/root`
points at sibling directories. `cljrsh prepare` downloads everything ahead
of time; the cache lives under `~/.cache/cljrsh/`.

## Pods

Pods are babashka's out-of-process plugin protocol, and cljrsh speaks it
unchanged, registry included:

```clojure
(require '[cljrsh.pods :as pods])
(pods/load-pod 'org.babashka/go-sqlite3 "0.2.4")
(require '[pod.babashka.go-sqlite3 :as sql])
(sql/execute! "app.db" ["create table if not exists t (x int)"])
```

The first load downloads the pod binary for your platform; later loads start
from the cache in ~70 ms. All three payload formats work (EDN, JSON,
transit), so existing pods like `go-sqlite3`, `postgresql`, and `datalevin`
run as-is. On that last one: `cljrsh.datalog` wraps the datalevin pod into
one-call datalog queries over plain collections:

```clojure
(require '[cljrsh.datalog :as d])
(def people [{:name "Ada"   :dept "eng" :salary 120}
             {:name "Grace" :dept "eng" :salary 130}
             {:name "Mary"  :dept "sci" :salary 125}])
(d/q '[:find ?n ?s
       :where [?e :dept "eng"] [?e :name ?n] [?e :salary ?s]]
     (d/facts people))
;; => #{["Ada" 120] ["Grace" 130]}
```

## Bundling with uberscript

`uberscript` concatenates a program and every source namespace it
transitively requires into one self-contained file:

```bash
$ cljrsh uberscript bundled.clj main.clj
Wrote bundled.clj (1 bundled namespace)
$ cd /anywhere && cljrsh bundled.clj hi
HI!
```

Built-in namespaces, pods, and native deps stay as plain `require`s; the
target runtime provides them. The result is the right artifact to drop into
a container or hand to a machine that has cljrsh but not your repository.

## Embedded nushell

The `nu` namespace runs nushell pipelines in-process and returns Clojure
data, no parsing involved:

```clojure
user=> (nu/eval "[[name size]; [a 1] [b 2]] | where size > 1 | get name")
["b"]
```

Structured shell operations (`ls`, `ps`, `open file.csv`, `http get`)
compose with everything else in this tutorial: a nushell table in, Clojure
data out.

## Where to next

With scripts and tasks in hand, the next three tutorials point them at
infrastructure: [Kubernetes](04-kubernetes.md), [AWS](05-aws.md), and
[Terraform](06-terraform.md).
