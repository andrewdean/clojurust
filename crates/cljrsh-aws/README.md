# cljrsh-aws

**Purpose:** Minimal data-driven AWS client — the `aws` Clojure namespace, Cognitect-aws-api-compatible: `(aws/client {:api :s3 ...})` + `(aws/invoke c {:op ... :request ...})` + `(aws/presign ...)`, anomaly maps on failure.

**Status:** Sized to real use (the causeway inventory): S3 rest-xml — GetObject/PutObject/DeleteObject/HeadObject/HeadBucket/CreateBucket/ListObjectsV2 + presigned GET — with **Garage/minio compatibility** (custom `:endpoint`, `:path-style` defaulting true when an endpoint is set, `AWS_ENDPOINT_URL` honored); plus a generic **awsJson** invoke for secretsmanager, dynamodb, sqs, ssm, logs, ecs, kinesis, states, eventbridge (any op passes through). Auth: explicit `:access-key-id`/`:secret-access-key`, env statics, else **IRSA** (`AWS_WEB_IDENTITY_TOKEN_FILE` + `AWS_ROLE_ARN` via unsigned `sts:AssumeRoleWithWebIdentity`, cached until near expiry). No profiles/SSO/IMDS. Everything beyond: pod-babashka-aws (verified working). Behind the binary's default-on `aws` feature.

## File layout

- `src/lib.rs` — `AwsClient` NativeObject, the `JSON_SERVICES` table, invoke dispatch (S3 vs awsJson), anomaly mapping, `aws/client|invoke|presign|ops` registration.
- `src/creds.rs` — the two-chain credential story (static/env, IRSA web-identity exchange with expiry cache).
- `src/wire.rs` — SigV4 signing (aws-sigv4; header + query/presign modes, S3 payload-hash + no path normalization), blocking HTTP on a dedicated thread (Send-only crossing), minimal XML helpers, URI encoding.
- `src/s3.rs` — per-op request planning (addressing: path-style/virtual-hosted × custom/AWS endpoints) and response shaping (`:Body` as byte array — `slurp` decodes it; ListObjectsV2 `:Contents` with `#inst` `:LastModified`).

## Failure shape

Non-2xx responses return data, aws-api style: `{:cognitect.anomalies/category :cognitect.anomalies/not-found, :StatusCode 404, :Code ..., :Message ...}` — scripts branch on `:cognitect.anomalies/category`.
