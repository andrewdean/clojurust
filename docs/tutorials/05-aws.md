# AWS

This tutorial covers the built-in `aws` namespace (cargo feature `aws`, on
by default): a minimal, data-driven AWS client with the cognitect aws-api
call shape. You need credentials for the live calls; the client resolves
them from explicit options, environment variables, or IRSA web identity, in
that order.

Coverage is deliberately sized to real use: S3 over rest-xml, presigned GET
URLs, and a generic invoke for the JSON-protocol services. Anything beyond
that falls back to `pod-babashka-aws`, which runs unchanged under cljrsh's
pod support.

## Clients and the invoke shape

One constructor, one entry point:

```clojure
(def s3 (aws/client {:api :s3 :region "us-east-1"}))

(aws/invoke s3 {:op :PutObject
                :request {:Bucket "acme-content" :Key "notes/today.txt"
                          :Body "hello"}})

(aws/invoke s3 {:op :ListObjectsV2
                :request {:Bucket "acme-content" :Prefix "notes/"}})
```

`aws/ops` lists what a client supports. For S3 that is:

```clojure
(keys (aws/ops s3))
;; => (:GetObject :PutObject :DeleteObject :HeadObject
;;     :HeadBucket :CreateBucket :ListObjectsV2)
```

Failures return anomaly maps rather than throwing, matching aws-api:
check for `:cognitect.anomalies/category` in the result.

## S3-compatible endpoints

`:endpoint` and `:path-style` point the client at Garage, MinIO, or any
S3-compatible store; SigV4 signing and payload hashing behave the same:

```clojure
(def garage (aws/client {:api :s3 :region "garage"
                         :endpoint "http://localhost:3900"
                         :path-style true}))
```

Static credentials come from `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`
(or `:access-key-id`/`:secret-access-key` in the client map); in-cluster,
IRSA web identity tokens are picked up and cached automatically.

## Presigned URLs

`aws/presign` signs a GET locally, no network call involved:

```clojure
(aws/presign garage {:op :GetObject
                     :request {:Bucket "media" :Key "cover.png"}
                     :expires 900})
;; => "http://localhost:3900/media/cover.png?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Cre..."
```

Hand the URL to a browser or another service; it grants that one object for
`:expires` seconds.

## JSON-protocol services

The same `invoke` reaches the awsJson services generically: Secrets
Manager, DynamoDB, SQS, SSM, CloudWatch Logs, ECS, Kinesis, Step Functions,
and EventBridge. Operations pass through by name, so any op those APIs
define is callable:

```clojure
(def sm (aws/client {:api :secretsmanager :region "us-east-1"}))

(-> (aws/invoke sm {:op :GetSecretValue
                    :request {:SecretId "prod/search/db-password"}})
    :SecretString)
```

For everything else (say, EC2's query protocol), load the pod:

```clojure
(require '[cljrsh.pods :as pods])
(pods/load-pod 'org.babashka/aws "0.1.2")
(require '[pod.babashka.aws :as pod-aws])
```

Both clients use the same request shape, so code migrates between them by
swapping the constructor.

## Where to next

[Terraform](06-terraform.md) closes the infrastructure loop: the buckets
and queues these calls touch are themselves declared as Clojure data.
