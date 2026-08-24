//! Minimal data-driven AWS client for cljrsh — the `aws` Clojure namespace.
//!
//! Cognitect-aws-api-compatible surface:
//!
//! ```clojure
//! (def s3 (aws/client {:api :s3 :region "us-east-1"
//!                      :endpoint "http://garage:3900" :path-style true}))
//! (aws/invoke s3 {:op :ListObjectsV2 :request {:Bucket "b" :Prefix "p/"}})
//! (aws/presign s3 {:op :GetObject :request {:Bucket "b" :Key "k"} :expires 900})
//! ```
//!
//! Coverage is deliberately sized to real use: S3 rest-xml (the seven ops +
//! presigned GET, Garage/path-style compatible) and a generic awsJson invoke
//! for the JSON-protocol services (Secrets Manager, DynamoDB, SQS, SSM, ...).
//! Anything beyond falls back to pod-babashka-aws. Auth: explicit/env static
//! keys, else IRSA web identity (cached). Failures return
//! `:cognitect.anomalies/*` maps, matching aws-api.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cljrs_env::env::GlobalEnv;
use cljrs_gc::{MarkVisitor, Trace};
use cljrs_interop::{Registry, wrap_fn_variadic};
use cljrs_value::value::MapValue;
use cljrs_value::{Keyword, NativeObject, Value, gc_native_object};

pub mod creds;
pub mod s3;
pub mod wire;

pub const NS: &str = "aws";

/// awsJson-protocol services callable through the generic invoke:
/// (api-name, endpoint-prefix, X-Amz-Target prefix, protocol version).
const JSON_SERVICES: &[(&str, &str, &str, &str)] = &[
    ("secretsmanager", "secretsmanager", "secretsmanager", "1.1"),
    ("dynamodb", "dynamodb", "DynamoDB_20120810", "1.0"),
    ("sqs", "sqs", "AmazonSQS", "1.0"),
    ("ssm", "ssm", "AmazonSSM", "1.1"),
    ("logs", "logs", "Logs_20140328", "1.1"),
    ("ecs", "ecs", "AmazonEC2ContainerServiceV20141113", "1.1"),
    ("kinesis", "kinesis", "Kinesis_20131202", "1.1"),
    ("states", "states", "AWSStepFunctions", "1.0"),
    ("eventbridge", "events", "AWSEvents", "1.1"),
];

pub struct AwsClient {
    api: String,
    region: String,
    endpoint: Option<String>,
    path_style: bool,
    explicit: Option<(String, String, Option<String>)>,
    cached: Mutex<Option<creds::Creds>>,
}

impl std::fmt::Debug for AwsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "AwsClient {{ api: {:?}, region: {:?} }}",
            self.api, self.region
        )
    }
}

impl Trace for AwsClient {
    fn trace(&self, _visitor: &mut MarkVisitor) {}
}

impl NativeObject for AwsClient {
    fn type_tag(&self) -> &str {
        "AwsClient"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl AwsClient {
    /// Static → env → IRSA (cached until near expiry).
    fn credentials(&self) -> Result<creds::Creds, String> {
        if let Some(c) = creds::static_creds(self.explicit.clone()) {
            return Ok(c);
        }
        if creds::irsa_available() {
            let mut cached = self.cached.lock().unwrap();
            if let Some(c) = cached.as_ref()
                && !c.expired()
            {
                return Ok(c.clone());
            }
            let fresh = creds::assume_role_with_web_identity(&self.region)?;
            *cached = Some(fresh.clone());
            return Ok(fresh);
        }
        Err(
            "no AWS credentials: set AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY, pass \
             :access-key-id/:secret-access-key in the client config, or run with IRSA \
             (AWS_WEB_IDENTITY_TOKEN_FILE + AWS_ROLE_ARN)"
                .to_string(),
        )
    }
}

// ── Value helpers ─────────────────────────────────────────────────────────────

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

fn as_client(v: &Value) -> Result<&AwsClient, String> {
    match v {
        Value::NativeObject(obj) => obj
            .get()
            .downcast_ref::<AwsClient>()
            .ok_or_else(|| "expected an aws client".to_string()),
        other => Err(format!("expected an aws client, got {}", other.type_name())),
    }
}

/// aws-api-style anomaly map for a failed exchange.
fn anomaly(status: u16, body: &[u8], json_api: bool) -> Value {
    let category = match status {
        401 | 403 => "cognitect.anomalies/forbidden",
        404 => "cognitect.anomalies/not-found",
        429 => "cognitect.anomalies/busy",
        400..=499 => "cognitect.anomalies/incorrect",
        503 => "cognitect.anomalies/unavailable",
        _ => "cognitect.anomalies/fault",
    };
    let mut m = MapValue::empty();
    m = m.assoc(
        Value::keyword(Keyword::parse("cognitect.anomalies/category")),
        Value::keyword(Keyword::parse(category)),
    );
    m = m.assoc(kw("StatusCode"), Value::Long(status as i64));
    let text = String::from_utf8_lossy(body);
    if json_api {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(t) = parsed.get("__type").and_then(|v| v.as_str()) {
                m = m.assoc(kw("Code"), Value::string(t.to_string()));
            }
            if let Some(t) = parsed
                .get("message")
                .or_else(|| parsed.get("Message"))
                .and_then(|v| v.as_str())
            {
                m = m.assoc(kw("Message"), Value::string(t.to_string()));
            }
        }
    } else {
        if let Some(code) = wire::xml_tag(&text, "Code") {
            m = m.assoc(kw("Code"), Value::string(code));
        }
        if let Some(msg) = wire::xml_tag(&text, "Message") {
            m = m.assoc(kw("Message"), Value::string(msg));
        }
    }
    Value::Map(m)
}

fn op_and_request(arg: &Value) -> Result<(String, MapValue), String> {
    let Value::Map(m) = arg else {
        return Err(format!(
            "invoke expects {{:op ... :request ...}}, got {}",
            arg.type_name()
        ));
    };
    let op = opt_str(m, "op").ok_or("invoke map needs :op")?;
    let request = match m.get(&kw("request")) {
        Some(Value::Map(r)) => r,
        None => MapValue::empty(),
        Some(other) => {
            return Err(format!(":request must be a map, got {}", other.type_name()));
        }
    };
    Ok((op, request))
}

// ── Invoke paths ──────────────────────────────────────────────────────────────

fn invoke_s3(client: &AwsClient, op: &str, request: &MapValue) -> Result<Value, String> {
    let target = s3::S3Target {
        region: client.region.clone(),
        endpoint: client.endpoint.clone(),
        path_style: client.path_style,
    };
    let (plan, shape) = s3::plan(op, request, &target)?;
    let creds = client.credentials()?;
    let ex = wire::execute(plan, creds)?;
    if (200..300).contains(&ex.status) {
        Ok(s3::shape_response(&shape, &ex))
    } else {
        Ok(anomaly(ex.status, &ex.body, false))
    }
}

fn invoke_json(
    client: &AwsClient,
    service: &(&str, &str, &str, &str),
    op: &str,
    request: &MapValue,
) -> Result<Value, String> {
    let (_, endpoint_prefix, target_prefix, version) = service;
    let endpoint = client
        .endpoint
        .clone()
        .unwrap_or_else(|| format!("https://{endpoint_prefix}.{}.amazonaws.com", client.region));
    let body = serde_json::to_vec(&cljrsh_host::json::value_to_json(&Value::Map(
        request.clone(),
    ))?)
    .map_err(|e| e.to_string())?;
    let plan = wire::Plan {
        method: "POST".to_string(),
        url: format!("{}/", endpoint.trim_end_matches('/')),
        headers: vec![
            (
                "content-type".to_string(),
                format!("application/x-amz-json-{version}"),
            ),
            ("x-amz-target".to_string(), format!("{target_prefix}.{op}")),
        ],
        body,
        service: endpoint_prefix.to_string(),
        region: client.region.clone(),
    };
    let creds = client.credentials()?;
    let ex = wire::execute(plan, creds)?;
    if (200..300).contains(&ex.status) {
        if ex.body.is_empty() {
            return Ok(Value::Map(MapValue::empty()));
        }
        let parsed: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&ex.body))
            .map_err(|e| format!("bad {endpoint_prefix} response JSON: {e}"))?;
        Ok(cljrsh_host::json::json_to_value(&parsed, true))
    } else {
        Ok(anomaly(ex.status, &ex.body, true))
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

/// Register the `aws` namespace. Idempotent.
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
        "aws/client",
        wrap_fn_variadic("aws/client", 1, |args: &[Value]| -> Result<Value, String> {
            let Value::Map(m) = &args[0] else {
                return Err("aws/client expects a config map".to_string());
            };
            let api = opt_str(m, "api").ok_or("client config needs :api")?;
            if api != "s3" && !JSON_SERVICES.iter().any(|(name, ..)| *name == api) {
                let known: Vec<&str> = std::iter::once("s3")
                    .chain(JSON_SERVICES.iter().map(|(n, ..)| *n))
                    .collect();
                return Err(format!(
                    ":api {api} is not built in (built-ins: {}); use pod-babashka-aws for the full surface",
                    known.join(" ")
                ));
            }
            let region = opt_str(m, "region")
                .or_else(|| std::env::var("AWS_REGION").ok())
                .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok())
                .ok_or("client config needs :region (or AWS_REGION)")?;
            let endpoint =
                opt_str(m, "endpoint").or_else(|| std::env::var("AWS_ENDPOINT_URL").ok());
            let path_style = match m.get(&kw("path-style")) {
                Some(Value::Bool(b)) => b,
                // Custom endpoints (Garage/minio) default to path style.
                _ => endpoint.is_some(),
            };
            let explicit = match (opt_str(m, "access-key-id"), opt_str(m, "secret-access-key")) {
                (Some(ak), Some(sk)) => Some((ak, sk, opt_str(m, "session-token"))),
                _ => None,
            };
            Ok(Value::NativeObject(gc_native_object(AwsClient {
                api,
                region,
                endpoint,
                path_style,
                explicit,
                cached: Mutex::new(None),
            })))
        }),
    );

    registry.define(
        "aws/invoke",
        wrap_fn_variadic("aws/invoke", 2, |args: &[Value]| -> Result<Value, String> {
            let client = as_client(&args[0])?;
            let (op, request) = op_and_request(&args[1])?;
            if client.api == "s3" {
                invoke_s3(client, &op, &request)
            } else {
                let service = JSON_SERVICES
                    .iter()
                    .find(|(name, ..)| *name == client.api)
                    .ok_or_else(|| format!("unknown api {}", client.api))?;
                invoke_json(client, service, &op, &request)
            }
        }),
    );

    registry.define(
        "aws/presign",
        wrap_fn_variadic(
            "aws/presign",
            2,
            |args: &[Value]| -> Result<Value, String> {
                let client = as_client(&args[0])?;
                if client.api != "s3" {
                    return Err("presign is only supported for :s3 clients".to_string());
                }
                let Value::Map(m) = &args[1] else {
                    return Err("presign expects {:op ... :request ... :expires secs}".to_string());
                };
                let (op, request) = op_and_request(&args[1])?;
                if op != "GetObject" {
                    return Err("presign currently supports :GetObject only".to_string());
                }
                let expires = match m.get(&kw("expires")) {
                    Some(Value::Long(secs)) => Duration::from_secs(secs as u64),
                    None => Duration::from_secs(900),
                    Some(other) => {
                        return Err(format!(
                            ":expires must be seconds, got {}",
                            other.type_name()
                        ));
                    }
                };
                let target = s3::S3Target {
                    region: client.region.clone(),
                    endpoint: client.endpoint.clone(),
                    path_style: client.path_style,
                };
                let (plan, _shape) = s3::plan(&op, &request, &target)?;
                let creds = client.credentials()?;
                wire::presign(&plan, &creds, expires).map(Value::string)
            },
        ),
    );

    // (aws/ops client) — the built-in op catalog for the client's api.
    registry.define(
        "aws/ops",
        wrap_fn_variadic("aws/ops", 1, |args: &[Value]| -> Result<Value, String> {
            let client = as_client(&args[0])?;
            let ops: Vec<&str> = if client.api == "s3" {
                vec![
                    "GetObject",
                    "PutObject",
                    "DeleteObject",
                    "HeadObject",
                    "HeadBucket",
                    "CreateBucket",
                    "ListObjectsV2",
                ]
            } else {
                vec![] // JSON apis are open-ended: any operation passes through.
            };
            let mut m = MapValue::empty();
            for op in ops {
                m = m.assoc(kw(op), Value::Map(MapValue::empty()));
            }
            Ok(Value::Map(m))
        }),
    );

    registry.env().mark_loaded(NS);
}
