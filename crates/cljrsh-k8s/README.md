# cljrsh-k8s

**Purpose:** Data-driven Kubernetes client — the `k8s` Clojure namespace on kube-rs's dynamic API: any resource (CRDs included) as plain Clojure maps, mirroring the `aws` namespace's `client` + `invoke` shape.

**Status:** Implemented and verified end-to-end against a live k3d cluster: get / list (label & field selectors, all-namespaces) / apply (server-side, field manager `cljrsh`, force) / patch (merge) / delete / logs / **exec** / **port-forward** (accept-loop bridging a local listener to per-connection API forwards, with stop handles) / raw API paths / api-resources discovery, plus `(k8s/secret c name)` — fetch + base64-decode `:data` in one call (the kubectl+jsonpath+base64 idiom). Auth: kubeconfig (`:context`, `:kubeconfig`) or in-cluster via `Config::infer`. Kind names resolve through live discovery by kind or plural, case-insensitive, `:group` to disambiguate. Behind the binary's default-on `k8s` feature. Not yet: watch, `kubectl kustomize` (shell out for rendering; `k8s/apply` the docs).

## File layout

- `src/lib.rs` — `K8sClient` NativeObject, invoke-map → command translation, `k8s/client` + `k8s/invoke` registration, and `K8S_SUGAR` (the Clojure sugar layer defined over invoke: get/list/apply/patch/delete/logs/exec/raw/port-forward/stop-forward/api-resources/secret).
- `src/worker.rs` — the per-client worker: one OS thread + current-thread Tokio runtime + kube `Client` + `Discovery` cache; commands/replies are `Send`-only (serde_json). rustls provider pinned to ring (both ring and aws-lc are in the workspace tree). Port-forwards run as background tasks with oneshot stop signals.

## Shape

```clojure
(def c (k8s/client {:context "causeway"}))
(k8s/list c :pods {:namespace "x" :label-selector "app=y"})
(k8s/apply c manifest-map)                          ; SSA, any CRD
(k8s/exec c "pod" ["sh" "-c" "..."] {:namespace "x"})
(let [f (k8s/port-forward c "pod" 80)] ... (k8s/stop-forward c f))
(:data (k8s/secret c "garage-s3-credentials"))      ; values decoded
```

Errors are catchable ex-info with the API server's reason/code/message.
