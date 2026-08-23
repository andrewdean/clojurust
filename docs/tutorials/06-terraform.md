# Terraform as data

Terraform natively reads JSON configuration, so a Terraform stack is
nothing but a map in the `*.tf.json` shape. The `tf` namespace leans into
that: every helper returns a plain map fragment, `tf/stack` merges
fragments with conflict detection, and the engine commands shell out to
OpenTofu or Terraform. You need `tofu` (the default) or `terraform` on
PATH; rebind `tf/*bin*` to switch.

## Fragments and stacks

Each block type has a constructor returning the corresponding map shape:

```clojure
(require '[tf])

(def stack
  (tf/stack
   (tf/terraform {:required_version ">= 1.6"})
   (tf/provider :aws {:region "us-east-1"})
   (tf/resource :aws_s3_bucket :content {:bucket "acme-content"})
   (tf/output :content-bucket {:value (tf/ref :aws_s3_bucket.content.bucket)})))

(println (tf/json stack))
```

```json
{
  "terraform": { "required_version": ">= 1.6" },
  "provider": { "aws": { "region": "us-east-1" } },
  "resource": { "aws_s3_bucket": { "content": { "bucket": "acme-content" } } },
  "output": { "content-bucket": { "value": "${aws_s3_bucket.content.bucket}" } }
}
```

Nothing is hidden behind a DSL: whatever the tf.json syntax allows, the map
allows. References are ordinary interpolation strings built by `tf/ref`
(dotted keyword or segments), `tf/var-ref`, `tf/local-ref`, or raw
`tf/expr`.

## Abstraction is function definition

Have you ever wanted `count` or `for_each` to just be your language's loop?
Here modules are functions. A parameterized pair of resources:

```clojure
(defn bucket [name* opts]
  (tf/stack
   (tf/resource :aws_s3_bucket name* {:bucket (str "acme-" (name name*))})
   (tf/resource :aws_s3_bucket_versioning name*
                {:bucket (tf/ref :aws_s3_bucket (name name*) :id)
                 :versioning_configuration
                 {:status (if (:versioned? opts) "Enabled" "Suspended")}})))

(def stack
  (tf/stack
   (tf/provider :aws {:region "us-east-1"})
   (bucket :content {:versioned? true})
   (bucket :logs    {:versioned? false})))
```

`tf/stack` merges recursively and throws on a colliding definition with the
offending path, so two fragments cannot silently define the same resource.

## The engine loop

`write!` emits `dir/main.tf.json`; the engine wrappers run the usual
lifecycle and throw with the tool's output on failure:

```clojure
(def dir "infra/dev")
(tf/write! dir stack)
(tf/init! dir)
(tf/validate! dir)
(tf/apply! dir {:vars {:env "prod"}})
(tf/output! dir)
;; => {:name {:sensitive false, :type "string", :value "acme-prod"}}
```

`tf/output!` parses `output -json`, so downstream code consumes outputs as
data. Variables pass as `{:vars {...}}` on `plan!` and `apply!`.

## Plans as data, policy as predicates

The payoff of the data-first approach: `tf/plan-json!` runs a plan and
returns the full `show -json` document, and `tf/changes` summarizes it:

```clojure
(def plan (tf/plan-json! dir {:vars {:env "staging"}}))

(tf/changes plan)
;; => {:create [...] :update [...] :delete [...] :replace [...]}
```

A policy gate is now a plain predicate over that map, reviewable and
testable like any other function:

```clojure
(let [{:keys [delete replace]} (tf/changes plan)]
  (when (seq (concat delete replace))
    (throw (ex-info "plan destroys resources; refusing to apply"
                    {:cljrsh/exit 3 :delete delete :replace replace}))))
```

Combined with tutorial 03's exit conventions, that snippet is a complete CI
gate: exit 3 with a one-line message and the offending addresses in the
data.

## Where to next

Stacks want configuration; [schemas](07-schemas.md) and
[layered configuration](08-configuration.md) show how to validate and
compose the maps that feed them.
