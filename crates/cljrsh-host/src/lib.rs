//! cljrsh host library surface.
//!
//! Native `cljrsh.*` namespaces implemented in Rust (fs, json — more per
//! milestone), plus the babashka-compatibility layer: thin portable-Clojure
//! veneers (`babashka.fs`, `cheshire.core`, `clojure.java.shell`,
//! `babashka.process`) registered as embedded builtin sources so they load
//! lazily on `require` (the `cljrs-stdlib` pattern).

use std::sync::Arc;

use cljrs_env::env::GlobalEnv;
use cljrs_interop::Registry;

pub mod csv;
pub mod dstore;
pub mod fs;
pub mod hash;
pub mod http;
pub mod io;
pub mod json;
pub mod term;
pub mod yaml;

/// Compat veneers shipped as embedded Clojure source, loaded on `require`.
const COMPAT_SOURCES: &[(&str, &str)] = &[
    // Primary cljrsh.* namespaces implemented in embedded Clojure (loaded on
    // require; the babashka.* entries below are compatibility shims over
    // them).
    ("cljrsh.process", include_str!("clj/cljrsh/process.cljrs")),
    (
        "borkdude.dynaload",
        include_str!("clj/borkdude/dynaload.cljrs"),
    ),
    ("cljrsh.wait", include_str!("clj/cljrsh/wait.cljrs")),
    ("cljrsh.http", include_str!("clj/cljrsh/http.cljrs")),
    ("cljrsh.datalog", include_str!("clj/cljrsh/datalog.cljrs")),
    ("cljrsh.config", include_str!("clj/cljrsh/config.cljrs")),
    // Vendored from org.babashka/cli 0.8.65 (EPL-1.0, github.com/babashka/cli)
    // verbatim — it runs unmodified on cljrsh's :clj-featured runtime.
    ("babashka.cli", include_str!("clj/babashka/cli.cljc")),
    (
        "babashka.cli.internal",
        include_str!("clj/babashka/cli_internal.cljc"),
    ),
    ("babashka.fs", include_str!("clj/babashka/fs.cljrs")),
    (
        "babashka.process",
        include_str!("clj/babashka/process.cljrs"),
    ),
    (
        "babashka.http-client",
        include_str!("clj/babashka/http_client.cljrs"),
    ),
    ("babashka.wait", include_str!("clj/babashka/wait.cljrs")),
    (
        "babashka.terminal",
        include_str!("clj/babashka/terminal.cljrs"),
    ),
    ("cheshire.core", include_str!("clj/cheshire/core.cljrs")),
    ("clj-yaml.core", include_str!("clj/clj_yaml/core.cljrs")),
    (
        "clojure.data.csv",
        include_str!("clj/clojure/data/csv.cljrs"),
    ),
    (
        "clojure.java.shell",
        include_str!("clj/clojure/java/shell.cljrs"),
    ),
    ("clojure.java.io", include_str!("clj/clojure/java/io.cljrs")),
    ("cljrs.dstore", include_str!("clj/cljrs/dstore.cljrs")),
    ("tf", include_str!("clj/tf.cljrs")),
    ("kustomize", include_str!("clj/kustomize.cljrs")),
    // Vendored from datascript 1.8.1 (EPL-1.0, github.com/tonsky/datascript)
    // with :cljrsh reader branches where the :clj branches assume the JVM.
    // The me.tonsky.persistent-sorted-set namespaces are cljrsh shims, not
    // vendored source.
    (
        "me.tonsky.persistent-sorted-set",
        include_str!("clj/me/tonsky/persistent_sorted_set.cljrs"),
    ),
    (
        "me.tonsky.persistent-sorted-set.arrays",
        include_str!("clj/me/tonsky/persistent_sorted_set/arrays.cljrs"),
    ),
    ("datascript.util", include_str!("clj/datascript/util.cljc")),
    ("datascript.lru", include_str!("clj/datascript/lru.cljc")),
    (
        "datascript.inline",
        include_str!("clj/datascript/inline.clj"),
    ),
    ("datascript.db", include_str!("clj/datascript/db.cljc")),
    (
        "datascript.parser",
        include_str!("clj/datascript/parser.cljc"),
    ),
    (
        "datascript.built-ins",
        include_str!("clj/datascript/built_ins.cljc"),
    ),
    (
        "datascript.pull-parser",
        include_str!("clj/datascript/pull_parser.cljc"),
    ),
    (
        "datascript.pull-api",
        include_str!("clj/datascript/pull_api.cljc"),
    ),
    (
        "datascript.impl.entity",
        include_str!("clj/datascript/impl/entity.cljc"),
    ),
    (
        "datascript.query",
        include_str!("clj/datascript/query.cljc"),
    ),
    // Vendored from datalevin 78b199e8 (EPL-2.0, github.com/juji-io/datalevin;
    // this repo is EPL-1.0 — both Eclipse licenses, version noted) with
    // :cljrsh reader branches replacing JVM interop. The engine port lands
    // namespace by namespace; storage-side code stays behind natives.
    ("datalevin.util", include_str!("clj/datalevin/util.clj")),
    (
        "datalevin.constants",
        include_str!("clj/datalevin/constants.clj"),
    ),
    ("datalevin.datom", include_str!("clj/datalevin/datom.clj")),
    (
        "datalevin.query-util",
        include_str!("clj/datalevin/query_util.clj"),
    ),
    ("datalevin.parser", include_str!("clj/datalevin/parser.clj")),
    (
        "datalevin.interface",
        include_str!("clj/datalevin/interface.clj"),
    ),
    (
        "datalevin.validate",
        include_str!("clj/datalevin/validate.clj"),
    ),
    (
        "datalevin.db.tx.common",
        include_str!("clj/datalevin/db/tx/common.clj"),
    ),
    ("datalevin.db", include_str!("clj/datalevin/db.clj")),
    ("datalevin.join", include_str!("clj/datalevin/join.clj")),
    ("datalevin.rules", include_str!("clj/datalevin/rules.clj")),
    (
        "datalevin.query.resolve",
        include_str!("clj/datalevin/query/resolve.clj"),
    ),
    (
        "datalevin.query.aggregate",
        include_str!("clj/datalevin/query/aggregate.clj"),
    ),
    (
        "datalevin.query.access",
        include_str!("clj/datalevin/query/access.clj"),
    ),
    (
        "datalevin.query.optimizer.range",
        include_str!("clj/datalevin/query/optimizer/range.clj"),
    ),
    (
        "datalevin.query.optimizer.graph",
        include_str!("clj/datalevin/query/optimizer/graph.clj"),
    ),
    (
        "datalevin.query.plan",
        include_str!("clj/datalevin/query/plan.clj"),
    ),
    (
        "datalevin.query-optimizer",
        include_str!("clj/datalevin/query_optimizer.clj"),
    ),
    (
        "datalevin.pull-parser",
        include_str!("clj/datalevin/pull_parser.clj"),
    ),
    (
        "datalevin.pull-api",
        include_str!("clj/datalevin/pull_api.clj"),
    ),
    (
        "datalevin.query.access.ave",
        include_str!("clj/datalevin/query/access/ave.clj"),
    ),
    (
        "datalevin.query.access.function",
        include_str!("clj/datalevin/query/access/function.clj"),
    ),
    (
        "datalevin.query.execute",
        include_str!("clj/datalevin/query/execute.clj"),
    ),
    (
        "datalevin.query.cache",
        include_str!("clj/datalevin/query/cache.clj"),
    ),
    ("datalevin.query", include_str!("clj/datalevin/query.clj")),
    (
        "datalevin.storage",
        include_str!("clj/datalevin/storage.clj"),
    ),
    (
        "datalevin.timeout",
        include_str!("clj/datalevin/timeout.clj"),
    ),
    ("datalevin.inline", include_str!("clj/datalevin/inline.clj")),
    ("datalevin.pipe", include_str!("clj/datalevin/pipe.clj")),
    (
        "datalevin.relation",
        include_str!("clj/datalevin/relation.clj"),
    ),
    (
        "datalevin.query.predicate",
        include_str!("clj/datalevin/query/predicate.clj"),
    ),
    (
        "datalevin.query.tuple",
        include_str!("clj/datalevin/query/tuple.clj"),
    ),
    (
        "datalevin.built-ins",
        include_str!("clj/datalevin/built_ins.clj"),
    ),
];

/// Register every native namespace and compat source into `globals`.
/// Idempotent (keyed on the `cljrsh.fs` namespace).
pub fn init(globals: &Arc<GlobalEnv>) {
    if globals.is_loaded("cljrsh.fs") {
        return;
    }
    for ns in [
        "cljrsh.fs",
        "cljrsh.json",
        "cljrsh.io",
        "cljrsh.http",
        "cljrsh.yaml",
        "cljrsh.csv",
        "cljrsh.term",
        "cljrsh.hash",
        "cljrs.dstore.native",
    ] {
        globals.get_or_create_ns(ns);
        globals.refer_all(ns, "clojure.core");
    }
    let mut registry = Registry::for_require(globals.clone());
    fs::register(&mut registry);
    json::register(&mut registry);
    io::register(&mut registry);
    http::register(&mut registry);
    yaml::register(&mut registry);
    csv::register(&mut registry);
    term::register(&mut registry);
    hash::register(&mut registry);
    dstore::register(&mut registry);
    globals.mark_loaded("cljrsh.fs");
    globals.mark_loaded("cljrsh.json");
    globals.mark_loaded("cljrsh.io");
    // cljrsh.http is NOT marked loaded: the native request* is interned
    // eagerly, and (require 'cljrsh.http) loads the rich layer source.
    globals.mark_loaded("cljrsh.yaml");
    globals.mark_loaded("cljrsh.csv");
    globals.mark_loaded("cljrsh.term");
    globals.mark_loaded("cljrsh.hash");
    globals.mark_loaded("cljrs.dstore.native");

    for (ns, src) in COMPAT_SOURCES {
        globals.register_builtin_source(ns, src);
    }
}
