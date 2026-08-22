# cljrsh-project

**Purpose:** bb.edn project-file support for cljrsh: discovery, parsing, and the task graph. Bodies stay unevaluated reader `Form`s; the binary evaluates them.

**Status:** Milestone B-M2 v1 — `:paths`, `:tasks` (expr / qualified-symbol / map bodies with `:task` `:doc` `:depends` `:private`), `:tasks`' `:init`, `:min-bb-version` capture, `cljrsh.edn`-wins discovery walking up from cwd, cycle-detecting `:depends` topological ordering. Milestone B-M3 adds `:deps`: `{:local/root}`, `{:git/url :git/sha}` (shallow-fetched via the `git` CLI into the cache), and `{:mvn/version}` via `src/maven.rs` — a minimal Clojars/Central fetcher with naive transitivity (compile-scope non-optional, first-declared-wins, same-pom property interpolation, dependencyManagement version lookups; version ranges are a hard error; parents are not consulted), sha-free download into a standard-layout cache, and jars unzipped once so `require` sees plain source directories (org.clojure/clojure + spec excluded — cljrsh ships core). Still later: `:pods`, task `:requires`/`:enter`/`:leave`, `--parallel`.

## File layout

- `src/lib.rs` — everything: `Project`/`TaskDef`/`ProjectError`, `find_project_file`, `load`, `task_order`, unit tests.

## Public API

- `find_project_file(start: &Path) -> Option<PathBuf>` — nearest `cljrsh.edn` (wins) or `bb.edn`, walking up.
- `load(path: &Path) -> Result<Project, ProjectError>` — parse via cljrs-reader in data mode; unknown keys ignored (babashka-style).
- `task_order(&Project, target) -> Result<Vec<&TaskDef>, ProjectError>` — transitive `:depends`, depth-first topological order, target last; `Cycle`/`UnknownTask` errors.
- `Project { root, file, paths, min_bb_version, tasks, init }`, `TaskDef { name, doc, depends, body: Form, private }`.

Execution semantics live in the binary (`crates/cljrsh/src/tasks.rs`): helpers `shell`/`sh` interned (and `clojure` errors — no JVM), `:init` evaluated once, each task's result interned under its name so downstream bodies reference upstream results, bare-symbol bodies called as fns.
