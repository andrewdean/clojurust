# Schemas with malli

Malli is the Clojure ecosystem's data-driven schema library, and version
0.17.0 runs unmodified from Clojars under cljrsh. This tutorial covers the
core workflow: declare a schema, validate, explain failures in human terms,
and coerce stringly input.

## Setup

Add malli to the project's `bb.edn`:

```clojure
{:deps {metosin/malli {:mvn/version "0.17.0"}}}
```

The first run downloads it; afterward it loads from the cache in
`~/.cache/cljrsh/`.

## Declare and validate

Schemas are data, so they live in code, EDN files, or registries alike:

```clojure
(require '[malli.core :as m])

(def Service
  [:map
   [:name :string]
   [:port [:int {:min 1 :max 65535}]]
   [:replicas {:optional true :default 1} :int]
   [:tags {:optional true} [:set :keyword]]])

(m/validate Service {:name "web" :port 8080})    ;; => true
(m/validate Service {:name "web" :port 99999})   ;; => false
```

Note the property placement: `{:optional true}` marks a key optional and
`{:default 1}` supplies a fill value for the transformer in the next
section. A key with only a `:default` is still required by `validate`.

## Explain failures for humans

`m/explain` returns the failure as data; `malli.error/humanize` turns it
into messages fit for a CLI or an HTTP 400 body:

```clojure
(require '[malli.error :as me])

(me/humanize (m/explain Service {:name "web" :port 99999}))
;; => {:port ["should be at most 65535"]}
```

The un-humanized explain document keeps paths and schema references, which
is the right input for programmatic handling; humanize is the last step
before a person sees it.

## Coerce strings and fill defaults

Input from the environment, CLI flags, and YAML arrives as strings. The
string transformer coerces them against the schema, and the default-value
transformer fills declared defaults:

```clojure
(require '[malli.transform :as mt])

(m/decode Service {:name "web" :port "8080"}
          (mt/transformer (mt/string-transformer)
                          (mt/default-value-transformer)))
;; => {:name "web", :port 8080}
```

Defaults on `:optional` keys are skipped unless you opt in:

```clojure
(m/decode Service {:name "web" :port "8080"}
          (mt/transformer
           (mt/string-transformer)
           (mt/default-value-transformer
            {:malli.transform/add-optional-keys true})))
;; => {:name "web", :port 8080, :replicas 1}
```

Decode-then-validate is the standard pipeline: coerce first, validate the
result, humanize on failure. Tutorial 08's `load-config` packages exactly
that pipeline for configuration maps.

## Schemas at the boundaries

Where do schemas pay for themselves? At every boundary a map crosses:

- **Task inputs**: validate `-x` argument maps before acting on them.
- **API responses**: check the shape of `k8s/get` or `aws/invoke` results
  that downstream code depends on.
- **Configuration**: the subject of the [next tutorial](08-configuration.md).
- **Data files**: EDN/JSON/YAML read from disk, validated at load time
  instead of failing three functions later.

A note on coverage: validation, explain/humanize, string coercion, and
defaults are exercised routinely under cljrsh. Function schemas
(`m/=>`, instrumentation) and generator-based testing are not yet part of
the tested surface; prefer the data-schema core shown here.
