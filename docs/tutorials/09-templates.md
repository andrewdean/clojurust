# Templating with data

Where other stacks reach for a template language (Jinja, Helm's Go
templates, ERB), the tutorials so far kept reaching for plain functions
over plain data. This closing tutorial names that pattern, shows the small
text-templating tools that exist for the residue, and explains when each
applies.

## The pattern: build data, then serialize

A template language interpolates strings and hopes the result parses. The
inverted approach builds the target structure as maps and vectors, with the
full language available, and serializes once at the edge:

```clojure
(require '[cljrsh.yaml :as yaml] '[cljrsh.json :as json])

(defn service-manifest [{:keys [name port replicas]}]
  {:apiVersion "v1"
   :kind "Service"
   :metadata {:name name :labels {:app name}}
   :spec {:ports [{:port port}]
          :selector {:app name}}})

(yaml/generate-string (service-manifest {:name "web" :port 8080}))
(json/generate-string (service-manifest {:name "web" :port 8080})
                      {:pretty true})
```

Loops are `map`, conditionals are `if`, includes are function calls, and a
malformed output is impossible because the serializer, not string
concatenation, produces it. The previous tutorials are all instances:
`kustomize/manifest` and `overlay` (04), the `tf` fragment builders (06),
and `cljrsh.config` layers (08) are this pattern specialized to a target
format.

Escaping bugs deserve one more sentence: a YAML value containing `: ` or a
JSON string containing a quote breaks a text template silently, while a
serializer quotes it correctly every time.

## Environments as overlays

The template-language idiom "one template, many value files" becomes "one
base map, one overlay per environment":

```clojure
(require '[kustomize])

(def envs
  {:dev     {:spec {:replicas 1}}
   :staging {:spec {:replicas 2}}
   :prod    {:spec {:replicas 6}}})

(defn render [env]
  (kustomize/overlay base-deployment (envs env)))
```

`kustomize/overlay` deep-merges maps, replaces everything else, and deletes
on nil, which covers the delta-from-base cases a values file handles, with
the conflict-checking alternative (`cljrsh.config/merge-layers`) available
when silent override is unacceptable.

## Text residue: format and clojure.template

Some output really is text: a report line, a MOTD, a code snippet.
`format` handles the aligned-columns cases:

```clojure
(format "%-8s %5.2f" "ratio" 0.6180339)
;; => "ratio     0.62"
```

`clojure.template` substitutes expressions into a form template,
which is occasionally the right tool for generating repetitive code or
test cases:

```clojure
(require '[clojure.template :as t])
(t/do-template [x y] (println (+ x y))
  1 2
  3 4)
;; prints 3, then 7
```

For multi-line text documents, `str` with `clojure.string/join` over a
vector of lines stays honest about structure:

```clojure
(defn motd [{:keys [host services]}]
  (clojure.string/join "\n"
    (concat [(str "== " host " ==") ""]
            (for [s services] (str "  * " s)))))
```

## Choosing a level

| Output | Reach for |
|--------|-----------|
| JSON or YAML document | Maps + `cljrsh.json`/`cljrsh.yaml` serializers |
| Kubernetes manifest | `kustomize/manifest` + `overlay` |
| Terraform configuration | `tf` fragments + `tf/stack` |
| Validated config map | `cljrsh.config/merge-layers` + `load-config` |
| Aligned text columns | `format` |
| Repetitive forms | `clojure.template` |
| Free-form text document | `clojure.string/join` over lines |

The row you pick matters less than the shared principle: keep the structure
in data for as long as possible, and let a serializer, not string
interpolation, produce the final bytes.

## The series, closed

You installed the binaries (01), explored interactively (02), scripted the
babashka way (03), and drove Kubernetes (04), AWS (05), and Terraform (06)
with maps in and maps out, validated by malli (07), composed by unification
(08), and rendered by serializers (09). The through-line is one idea:
everything is data, and the standard library is the template engine, the
policy engine, and the query language all at once.
