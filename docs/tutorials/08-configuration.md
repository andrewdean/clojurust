# Layered configuration

Configuration usually accretes: a base file, an environment profile, a few
environment-variable escapes. The common failure mode is silent: a later
layer overwrites an earlier one nobody meant to change. `cljrsh.config`
borrows CUE's answer: merging is unification, and a genuine conflict is an
error unless a layer says otherwise.

## Unification, not overwriting

`merge-layers` deep-merges maps and accepts agreeing values; two layers that
disagree on a scalar throw, with the path:

```clojure
(require '[cljrsh.config :as cfg])

(def base {:service {:name "search" :port 8080} :log-level "info"})
(def prod {:service {:replicas 3}               :log-level (cfg/override "warn")})

(cfg/merge-layers base prod)
;; => {:service {:name "search", :port 8080, :replicas 3}, :log-level "warn"}

(cfg/merge-layers {:a 1} {:a 2})
;; => throws: config conflict at [:a]: 1 vs 2
;;    (wrap one in cljrsh.config/override to allow)
```

`override` marks the one place a layer deliberately replaces an earlier
value. In other words, precedence is visible in the data itself: reading a
profile tells you exactly where it disagrees with the base, because every
disagreement is annotated.

## Schema-checked loading

`load-config` composes the layers and then applies a malli schema in one
call: defaults fill missing keys, strings coerce to schema types, and
failures throw with humanized errors in the ex-data. Add
`{:deps {metosin/malli {:mvn/version "0.17.0"}}}` to bb.edn (see
[tutorial 07](07-schemas.md)); `merge-layers` and `override` work without
it.

```clojure
(def Config
  [:map
   [:service [:map
              [:name :string]
              [:port :int]
              [:replicas {:default 1} :int]]]
   [:log-level [:enum "debug" "info" "warn" "error"]]])

(def env-layer
  {:service {:port (or (System/getenv "PORT") "9090")}})

(cfg/load-config [base {:service {:port (cfg/override nil)}} env-layer]
                 Config)
;; => {:service {:name "search", :port 9090, :replicas 1}, :log-level "info"}
```

Walk through what happened: the middle layer retracts the base port with
`(override nil)`, the environment layer supplies `"9090"` as a string, the
string transformer coerces it to `9090`, and `:replicas` fills from its
default. String coercion is what makes environment variables first-class
layer material; disable it with `{:coerce? false}` when layers are already
typed.

A failing value reports in human terms:

```clojure
(try
  (cfg/load-config [{:service {:name "x" :port "not-a-port"}
                     :log-level "info"}] Config)
  (catch Exception e
    (:errors (ex-data e))))
;; => {:service {:port ["should be an integer"]}}
```

## A working layout

A pattern that scales from one script to a repository of tasks:

```clojure
(defn config []
  (cfg/load-config
   [(clojure.edn/read-string (slurp "config/base.edn"))
    (when-let [env (System/getenv "APP_ENV")]
      (clojure.edn/read-string (slurp (str "config/" env ".edn"))))
    {:service {:port (System/getenv "PORT")}}]   ;; nil values unify away
   Config))
```

Layers are ordered general to specific: base file, environment profile,
process environment. `merge-layers` skips nil layers, and nil map values
unify with anything, so absent environment variables need no special
casing. Profiles stay small because they only state their differences, and
any accidental collision between profiles surfaces as a thrown conflict at
load time rather than a silently wrong value at runtime.

## Where to next

Validated config maps feed the [Terraform stacks](06-terraform.md) and
[Kubernetes manifests](04-kubernetes.md) from earlier tutorials; the
[final tutorial](09-templates.md) generalizes the pattern to any file you
would otherwise reach for a template language to produce.
