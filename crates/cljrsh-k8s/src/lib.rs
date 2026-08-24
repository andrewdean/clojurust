//! Data-driven Kubernetes client for cljrsh — the `k8s` Clojure namespace.
//!
//! kube-rs dynamic API: any resource (CRDs included) as plain Clojure maps,
//! kubeconfig-context or in-cluster auth. The invoke shape mirrors the `aws`
//! namespace's data-driven surface:
//!
//! ```clojure
//! (def c (k8s/client {:context "causeway"}))
//! (k8s/invoke c {:op :list :kind :pods :namespace "x" :label-selector "app=y"})
//! ```
//!
//! Sugar covers the script workhorses: get/list/apply(SSA)/patch/delete/
//! logs/exec/port-forward/raw/api-resources.

use std::sync::Arc;

use cljrs_env::env::GlobalEnv;
use cljrs_gc::{MarkVisitor, Trace};
use cljrs_interop::{Registry, wrap_fn_variadic};
use cljrs_value::value::MapValue;
use cljrs_value::{Keyword, NativeObject, Value, gc_native_object};

pub mod worker;

use worker::{Cmd, KindRef, WorkerHandle};

pub const NS: &str = "k8s";

pub struct K8sClient {
    worker: WorkerHandle,
}

impl std::fmt::Debug for K8sClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "K8sClient")
    }
}

impl Trace for K8sClient {
    fn trace(&self, _visitor: &mut MarkVisitor) {}
}

impl NativeObject for K8sClient {
    fn type_tag(&self) -> &str {
        "K8sClient"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn kw(name: &str) -> Value {
    Value::keyword(Keyword::simple(name))
}

fn opt_str(m: &MapValue, key: &str) -> Option<String> {
    match m.get(&kw(key)) {
        Some(Value::Str(s)) => Some(s.get().to_string()),
        Some(Value::Keyword(k)) => Some(k.get().name.to_string()),
        _ => None,
    }
}

fn opt_long(m: &MapValue, key: &str) -> Option<i64> {
    match m.get(&kw(key)) {
        Some(Value::Long(n)) => Some(n),
        _ => None,
    }
}

fn as_client(v: &Value) -> Result<&K8sClient, String> {
    match v {
        Value::NativeObject(obj) => obj
            .get()
            .downcast_ref::<K8sClient>()
            .ok_or_else(|| "expected a k8s client".to_string()),
        other => Err(format!("expected a k8s client, got {}", other.type_name())),
    }
}

fn kind_ref(m: &MapValue) -> Result<KindRef, String> {
    let name =
        opt_str(m, "kind").ok_or("needs :kind (e.g. :pods, :Deployment, :CausewayWorkspace)")?;
    Ok(KindRef {
        name,
        group: opt_str(m, "group"),
    })
}

/// Build the worker command from an invoke map.
fn command(m: &MapValue) -> Result<Cmd, String> {
    let op = opt_str(m, "op").ok_or("invoke map needs :op")?;
    let ns = opt_str(m, "namespace");
    Ok(match op.as_str() {
        "get" => Cmd::Get {
            kind: kind_ref(m)?,
            name: opt_str(m, "name").ok_or(":get needs :name")?,
            ns,
        },
        "list" => Cmd::List {
            kind: kind_ref(m)?,
            ns,
            all_namespaces: matches!(m.get(&kw("all-namespaces")), Some(Value::Bool(true))),
            label_selector: opt_str(m, "label-selector"),
            field_selector: opt_str(m, "field-selector"),
        },
        "apply" => {
            let manifest = m.get(&kw("manifest")).ok_or(":apply needs :manifest")?;
            Cmd::Apply {
                manifest: cljrsh_host::json::value_to_json(&manifest)?,
            }
        }
        "patch" => {
            let patch = m.get(&kw("patch")).ok_or(":patch needs :patch")?;
            Cmd::PatchMerge {
                kind: kind_ref(m)?,
                name: opt_str(m, "name").ok_or(":patch needs :name")?,
                ns,
                patch: cljrsh_host::json::value_to_json(&patch)?,
            }
        }
        "delete" => Cmd::Delete {
            kind: kind_ref(m)?,
            name: opt_str(m, "name").ok_or(":delete needs :name")?,
            ns,
        },
        "logs" => Cmd::Logs {
            name: opt_str(m, "name").ok_or(":logs needs :name")?,
            ns,
            container: opt_str(m, "container"),
            tail: opt_long(m, "tail"),
        },
        "exec" => {
            let command = match m.get(&kw("command")) {
                Some(Value::Vector(v)) => v
                    .get()
                    .iter()
                    .map(|e| match &e {
                        Value::Str(s) => Ok(s.get().to_string()),
                        other => Err(format!(
                            "command elements must be strings, got {}",
                            other.type_name()
                        )),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                _ => return Err(":exec needs :command [\"sh\" \"-c\" ...]".to_string()),
            };
            Cmd::Exec {
                name: opt_str(m, "name").ok_or(":exec needs :name")?,
                ns,
                container: opt_str(m, "container"),
                command,
            }
        }
        "raw" => Cmd::Raw {
            path: opt_str(m, "path").ok_or(":raw needs :path")?,
        },
        "port-forward" => Cmd::PortForward {
            name: opt_str(m, "name").ok_or(":port-forward needs :name")?,
            ns,
            remote: opt_long(m, "port").ok_or(":port-forward needs :port")? as u16,
            local: opt_long(m, "local-port").map(|p| p as u16),
        },
        "stop-forward" => Cmd::StopForward {
            id: opt_long(m, "id").ok_or(":stop-forward needs :id")? as u64,
        },
        "api-resources" => Cmd::ApiResources,
        other => {
            return Err(format!(
                "unsupported k8s op :{other} (supported: get list apply patch delete logs exec raw port-forward stop-forward api-resources)"
            ));
        }
    })
}

fn invoke(client: &K8sClient, arg: &Value) -> Result<Value, String> {
    let Value::Map(m) = arg else {
        return Err(format!("invoke expects a map, got {}", arg.type_name()));
    };
    let cmd = command(m)?;
    let json = client.worker.call(cmd)?;
    Ok(cljrsh_host::json::json_to_value(&json, true))
}

/// Register the `k8s` namespace. Idempotent.
pub fn init(globals: &Arc<GlobalEnv>) {
    if globals.is_loaded(NS) {
        return;
    }
    globals.get_or_create_ns(NS);
    globals.refer_all(NS, "clojure.core");
    let mut registry = Registry::for_require(globals.clone());
    register(&mut registry);
}

pub fn register(registry: &mut Registry) {
    registry.define(
        "k8s/client",
        wrap_fn_variadic("k8s/client", 0, |args: &[Value]| -> Result<Value, String> {
            let (context, kubeconfig) = match args.first() {
                Some(Value::Map(m)) => (opt_str(m, "context"), opt_str(m, "kubeconfig")),
                _ => (None, None),
            };
            let worker = worker::spawn(context, kubeconfig)?;
            Ok(Value::NativeObject(gc_native_object(K8sClient { worker })))
        }),
    );

    registry.define(
        "k8s/invoke",
        wrap_fn_variadic("k8s/invoke", 2, |args: &[Value]| -> Result<Value, String> {
            invoke(as_client(&args[0])?, &args[1])
        }),
    );

    registry.env().mark_loaded(NS);
}

/// Sugar fns are defined in Clojure over `k8s/invoke` (see K8S_SUGAR).
pub const K8S_SUGAR: &str = r#"
(defn get [c kind name & [{:keys [namespace group]}]]
  (invoke c {:op :get :kind kind :name name :namespace namespace :group group}))

(defn list [c kind & [{:keys [namespace all-namespaces label-selector field-selector group]}]]
  (invoke c {:op :list :kind kind :namespace namespace :all-namespaces all-namespaces
             :label-selector label-selector :field-selector field-selector :group group}))

(defn apply [c manifest]
  (invoke c {:op :apply :manifest manifest}))

(defn patch [c kind name patch-map & [{:keys [namespace group]}]]
  (invoke c {:op :patch :kind kind :name name :patch patch-map
             :namespace namespace :group group}))

(defn delete [c kind name & [{:keys [namespace group]}]]
  (invoke c {:op :delete :kind kind :name name :namespace namespace :group group}))

(defn logs [c pod & [{:keys [namespace container tail]}]]
  (invoke c {:op :logs :name pod :namespace namespace :container container :tail tail}))

(defn exec [c pod command & [{:keys [namespace container]}]]
  (invoke c {:op :exec :name pod :command command
             :namespace namespace :container container}))

(defn raw [c path]
  (invoke c {:op :raw :path path}))

(defn port-forward [c pod port & [{:keys [namespace local-port]}]]
  (invoke c {:op :port-forward :name pod :port port
             :namespace namespace :local-port local-port}))

(defn stop-forward [c handle]
  (invoke c {:op :stop-forward :id (:id handle)}))

(defn api-resources [c]
  (invoke c {:op :api-resources}))

(defn secret
  "Fetch a Secret and base64-decode its :data values (the kubectl
  get-secret + jsonpath + base64 -d idiom, as one call)."
  [c name & [opts]]
  (let [s (get c :Secret name opts)]
    (update s :data
            (fn [data]
              (into {} (map (fn [[k v]] [k (slurp (cljrs.base64/decode v))]) data))))))
"#;
