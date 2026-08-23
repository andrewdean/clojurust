# Kubernetes

This tutorial drives a cluster from cljrsh: reading and writing resources,
composing manifests as data, and interoperating with kustomize trees. You
need a reachable cluster (the examples ran against a local k3d cluster) and,
for the kustomize sections, `kubectl` on PATH.

The `k8s` namespace is built in (cargo feature `k8s`, on by default). It
rides kube-rs's dynamic API, so every resource kind works, custom resources
included, and everything is plain Clojure maps in both directions.

## Connect and read

`k8s/client` picks up your kubeconfig; pass `:context` or `:kubeconfig` to
choose one explicitly:

```clojure
(def c (k8s/client {:context "k3d-localauth"}))

(doseq [p (k8s/list c :Pod {:namespace "kube-system"})]
  (println (get-in p [:metadata :name]) "->" (get-in p [:status :phase])))
```

```
coredns-9cb6448f4-ldl5g -> Running
local-path-provisioner-5cf85fd84d-zjh9w -> Running
metrics-server-5985cbc9d7-n29cv -> Running
```

`k8s/list` returns the vector of items directly. Selectors narrow it:

```clojure
(first (k8s/list c :Pod {:namespace "kube-system"
                         :label-selector "k8s-app=kube-dns"}))
```

## Write: apply, patch, delete

Manifests are maps; `k8s/apply` is server-side apply, so the same call
creates and updates:

```clojure
(k8s/apply c {:apiVersion "v1" :kind "ConfigMap"
              :metadata {:name "tutorial-demo" :namespace "default"}
              :data {"greeting" "hello"}})

(get-in (k8s/get c :ConfigMap "tutorial-demo" {:namespace "default"})
        [:data :greeting])
;; => "hello"

(k8s/patch c :ConfigMap "tutorial-demo"
           {:data {"greeting" "bonjour"}} {:namespace "default"})

(k8s/delete c :ConfigMap "tutorial-demo" {:namespace "default"})
```

Group-qualified and custom resources take a `:group`; `k8s/api-resources`
lists what the cluster serves, and discovery refreshes on a miss, so a CRD
installed after the client connected still resolves.

## The op map underneath

Every sugar function delegates to one data-driven entry point,
`(k8s/invoke c {:op ...})`, with ops `:get :list :apply :patch :delete
:logs :exec :raw :port-forward :stop-forward :api-resources`. Scripts that
build operations programmatically use it directly; everything else reads
better through the sugar.

Three ops deserve a note:

```clojure
;; logs and exec, the kubectl staples:
(k8s/logs c "coredns-9cb6448f4-ldl5g" {:namespace "kube-system" :tail 5})
(k8s/exec c "mypod" ["cat" "/etc/hostname"] {:namespace "default"})

;; secrets, base64-decoded in one call:
(get-in (k8s/secret c "tut-secret" {:namespace "default"})
        [:data :password])
;; => "hunter2"

;; a tunnel for HTTP against an in-cluster service:
(def fwd (k8s/port-forward c "mypod" 8080 {:namespace "default"}))
;; ... (cljrsh.http/get (str "http://localhost:" (:local-port fwd) "/health"))
(k8s/stop-forward c fwd)
```

## Manifests as functions

Because manifests are maps, the `kustomize` namespace treats overlays as
function composition. `kustomize/manifest` builds the skeleton and
`kustomize/overlay` is a strategic-merge-lite: maps deep-merge, `nil`
deletes a key, anything else replaces:

```clojure
(require '[kustomize])

(def base
  (kustomize/manifest "apps/v1" :Deployment "web" {:labels {:app "web"}}
    {:replicas 1
     :selector {:matchLabels {:app "web"}}
     :template {:metadata {:labels {:app "web"}}
                :spec {:containers [{:name "web" :image "nginx:1.27"}]}}}))

(def prod (kustomize/overlay base {:spec {:replicas 3}}))
(get-in prod [:spec :replicas])   ;; => 3

(k8s/apply c prod)
```

No patch files, no YAML templating: an environment is a function from base
manifest to variant, and `let`, `map`, and `merge` are the overlay language.

## Kustomize interop

For existing GitOps trees, the same namespace round-trips through real
kustomize. `write!` emits a kustomize-compatible directory and `build`
renders any kustomization back into data via `kubectl kustomize`:

```clojure
(kustomize/write! "kustdemo"
  {:kustomization {:namespace "staging" :commonLabels {:team "search"}}
   :resources {"deploy.yaml" base}})

(def rendered (kustomize/build "kustdemo"))
(get-in (first rendered) [:metadata :namespace])   ;; => "staging"
(get-in (first rendered) [:metadata :labels])      ;; => {:app "web", :team "search"}

(kustomize/apply-all! c rendered)
```

Use mode one (overlay functions) for new code and mode two (`write!` /
`build`) where a kustomize directory is the contract with other tooling.

## Where to next

The same maps-in, maps-out pattern extends to [AWS](05-aws.md) and to
[Terraform](06-terraform.md), where the whole stack becomes one data
structure.
