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
pub mod fs;
pub mod http;
pub mod io;
pub mod json;
pub mod yaml;

/// Compat veneers shipped as embedded Clojure source, loaded on `require`.
const COMPAT_SOURCES: &[(&str, &str)] = &[
    ("babashka.fs", include_str!("clj/babashka/fs.cljrs")),
    ("babashka.process", include_str!("clj/babashka/process.cljrs")),
    (
        "babashka.http-client",
        include_str!("clj/babashka/http_client.cljrs"),
    ),
    ("cheshire.core", include_str!("clj/cheshire/core.cljrs")),
    ("clj-yaml.core", include_str!("clj/clj_yaml/core.cljrs")),
    ("clojure.data.csv", include_str!("clj/clojure/data/csv.cljrs")),
    (
        "clojure.java.shell",
        include_str!("clj/clojure/java/shell.cljrs"),
    ),
];

/// Register every native namespace and compat source into `globals`.
/// Idempotent (keyed on the `cljrsh.fs` namespace).
pub fn init(globals: &Arc<GlobalEnv>) {
    if globals.is_loaded("cljrsh.fs") {
        return;
    }
    for ns in ["cljrsh.fs", "cljrsh.json", "cljrsh.io", "cljrsh.http", "cljrsh.yaml", "cljrsh.csv"] {
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
    globals.mark_loaded("cljrsh.fs");
    globals.mark_loaded("cljrsh.json");
    globals.mark_loaded("cljrsh.io");
    globals.mark_loaded("cljrsh.http");
    globals.mark_loaded("cljrsh.yaml");
    globals.mark_loaded("cljrsh.csv");

    for (ns, src) in COMPAT_SOURCES {
        globals.register_builtin_source(ns, src);
    }
}
