//! S3 operation planning (rest-xml) and response shaping — the seven
//! operations causeway uses, plus presigned GET. Garage-compatible:
//! custom endpoints and path-style addressing.

use std::collections::BTreeMap;

use cljrs_gc::GcPtr;
use cljrs_value::value::MapValue;
use cljrs_value::{Keyword, PersistentVector, Value};

use crate::wire::{self, Plan};

pub struct S3Target {
    pub region: String,
    pub endpoint: Option<String>,
    pub path_style: bool,
}

fn kw(name: &str) -> Value {
    Value::keyword(Keyword::simple(name))
}

fn get_str(req: &MapValue, key: &str) -> Option<String> {
    match req.get(&kw(key)) {
        Some(Value::Str(s)) => Some(s.get().to_string()),
        _ => None,
    }
}

fn get_long(req: &MapValue, key: &str) -> Option<i64> {
    match req.get(&kw(key)) {
        Some(Value::Long(n)) => Some(n),
        _ => None,
    }
}

/// (endpoint base, path prefix) for a bucket under this target.
fn addressing(t: &S3Target, bucket: &str) -> (String, String) {
    match (&t.endpoint, t.path_style) {
        (Some(ep), true) => (ep.clone(), format!("/{bucket}")),
        (Some(ep), false) => {
            // Virtual-hosted against a custom endpoint: bucket label prefix.
            let ep = ep.replace("://", &format!("://{bucket}."));
            (ep, String::new())
        }
        (None, true) => (
            format!("https://s3.{}.amazonaws.com", t.region),
            format!("/{bucket}"),
        ),
        (None, false) => (
            format!("https://{bucket}.s3.{}.amazonaws.com", t.region),
            String::new(),
        ),
    }
}

/// Build the HTTP plan for one S3 op. Returns the plan plus how to shape the
/// successful response.
pub fn plan(op: &str, req: &MapValue, t: &S3Target) -> Result<(Plan, Shape), String> {
    let bucket = get_str(req, "Bucket").ok_or_else(|| format!("{op} needs :Bucket"))?;
    let (base, prefix) = addressing(t, &bucket);
    let key_path = |key: &str| format!("{prefix}/{}", wire::uri_encode(key, false));
    let mut query: BTreeMap<String, String> = BTreeMap::new();
    let mk = |method: &str, url: String, headers: Vec<(String, String)>, body: Vec<u8>| Plan {
        method: method.to_string(),
        url,
        headers,
        body,
        service: "s3".to_string(),
        region: t.region.clone(),
    };

    Ok(match op {
        "GetObject" => {
            let key = get_str(req, "Key").ok_or("GetObject needs :Key")?;
            (
                mk(
                    "GET",
                    wire::build_url(&base, &key_path(&key), &query),
                    vec![],
                    vec![],
                ),
                Shape::Object,
            )
        }
        "HeadObject" => {
            let key = get_str(req, "Key").ok_or("HeadObject needs :Key")?;
            (
                mk(
                    "HEAD",
                    wire::build_url(&base, &key_path(&key), &query),
                    vec![],
                    vec![],
                ),
                Shape::Head,
            )
        }
        "DeleteObject" => {
            let key = get_str(req, "Key").ok_or("DeleteObject needs :Key")?;
            (
                mk(
                    "DELETE",
                    wire::build_url(&base, &key_path(&key), &query),
                    vec![],
                    vec![],
                ),
                Shape::Empty,
            )
        }
        "PutObject" => {
            let key = get_str(req, "Key").ok_or("PutObject needs :Key")?;
            let body = match req.get(&kw("Body")) {
                Some(Value::Str(s)) => s.get().as_bytes().to_vec(),
                Some(Value::ByteArray(b)) => {
                    b.get().lock().unwrap().iter().map(|&x| x as u8).collect()
                }
                None => Vec::new(),
                Some(other) => {
                    return Err(format!(
                        ":Body must be a string or byte array, got {}",
                        other.type_name()
                    ));
                }
            };
            let mut headers = Vec::new();
            if let Some(ct) = get_str(req, "ContentType") {
                headers.push(("content-type".to_string(), ct));
            }
            (
                mk(
                    "PUT",
                    wire::build_url(&base, &key_path(&key), &query),
                    headers,
                    body,
                ),
                Shape::Put,
            )
        }
        "HeadBucket" => (
            mk(
                "HEAD",
                wire::build_url(&base, &format!("{prefix}/"), &query),
                vec![],
                vec![],
            ),
            Shape::Head,
        ),
        "CreateBucket" => {
            let body = if t.region == "us-east-1" || t.endpoint.is_some() {
                Vec::new()
            } else {
                format!(
                    "<CreateBucketConfiguration><LocationConstraint>{}</LocationConstraint></CreateBucketConfiguration>",
                    wire::xml_escape(&t.region)
                )
                .into_bytes()
            };
            (
                mk(
                    "PUT",
                    wire::build_url(&base, &format!("{prefix}/"), &query),
                    vec![],
                    body,
                ),
                Shape::Empty,
            )
        }
        "ListObjectsV2" => {
            query.insert("list-type".to_string(), "2".to_string());
            if let Some(p) = get_str(req, "Prefix") {
                query.insert("prefix".to_string(), p);
            }
            if let Some(d) = get_str(req, "Delimiter") {
                query.insert("delimiter".to_string(), d);
            }
            if let Some(tok) = get_str(req, "ContinuationToken") {
                query.insert("continuation-token".to_string(), tok);
            }
            if let Some(n) = get_long(req, "MaxKeys") {
                query.insert("max-keys".to_string(), n.to_string());
            }
            (
                mk(
                    "GET",
                    wire::build_url(&base, &format!("{prefix}/"), &query),
                    vec![],
                    vec![],
                ),
                Shape::List,
            )
        }
        other => {
            return Err(format!(
                "unsupported s3 op :{other} (supported: GetObject PutObject DeleteObject HeadObject HeadBucket CreateBucket ListObjectsV2; use pod-babashka-aws for the full surface)"
            ));
        }
    })
}

/// How to turn a successful exchange into a Clojure value.
pub enum Shape {
    Object,
    Head,
    Put,
    Empty,
    List,
}

fn header<'a>(ex: &'a wire::Exchange, name: &str) -> Option<&'a str> {
    ex.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn bytes_value(bytes: &[u8]) -> Value {
    Value::ByteArray(GcPtr::new(std::sync::Mutex::new(
        bytes.iter().map(|&b| b as i8).collect(),
    )))
}

pub fn shape_response(shape: &Shape, ex: &wire::Exchange) -> Value {
    let mut m = MapValue::empty();
    match shape {
        Shape::Empty => {}
        Shape::Put => {
            if let Some(etag) = header(ex, "etag") {
                m = m.assoc(kw("ETag"), Value::string(etag.to_string()));
            }
        }
        Shape::Object => {
            m = m.assoc(kw("Body"), bytes_value(&ex.body));
            if let Some(ct) = header(ex, "content-type") {
                m = m.assoc(kw("ContentType"), Value::string(ct.to_string()));
            }
            if let Some(len) = header(ex, "content-length").and_then(|v| v.parse::<i64>().ok()) {
                m = m.assoc(kw("ContentLength"), Value::Long(len));
            }
            if let Some(etag) = header(ex, "etag") {
                m = m.assoc(kw("ETag"), Value::string(etag.to_string()));
            }
        }
        Shape::Head => {
            if let Some(len) = header(ex, "content-length").and_then(|v| v.parse::<i64>().ok()) {
                m = m.assoc(kw("ContentLength"), Value::Long(len));
            }
            if let Some(ct) = header(ex, "content-type") {
                m = m.assoc(kw("ContentType"), Value::string(ct.to_string()));
            }
            if let Some(etag) = header(ex, "etag") {
                m = m.assoc(kw("ETag"), Value::string(etag.to_string()));
            }
            if let Some(lm) = header(ex, "last-modified") {
                m = m.assoc(kw("LastModified"), Value::string(lm.to_string()));
            }
        }
        Shape::List => {
            let xml = String::from_utf8_lossy(&ex.body);
            let contents: Vec<Value> = wire::xml_blocks(&xml, "Contents")
                .into_iter()
                .map(|block| {
                    let mut e = MapValue::empty();
                    if let Some(k) = wire::xml_tag(block, "Key") {
                        e = e.assoc(kw("Key"), Value::string(k));
                    }
                    if let Some(sz) =
                        wire::xml_tag(block, "Size").and_then(|s| s.parse::<i64>().ok())
                    {
                        e = e.assoc(kw("Size"), Value::Long(sz));
                    }
                    if let Some(t) = wire::xml_tag(block, "ETag") {
                        e = e.assoc(kw("ETag"), Value::string(t));
                    }
                    if let Some(lm) = wire::xml_tag(block, "LastModified") {
                        if let Ok(ms) = cljrs_types::instant::parse_rfc3339_millis(&lm) {
                            e = e.assoc(kw("LastModified"), Value::Instant(ms));
                        } else {
                            e = e.assoc(kw("LastModified"), Value::string(lm));
                        }
                    }
                    Value::Map(e)
                })
                .collect();
            m = m.assoc(
                kw("Contents"),
                Value::Vector(GcPtr::new(PersistentVector::from_iter(contents))),
            );
            if let Some(kc) = wire::xml_tag(&xml, "KeyCount").and_then(|s| s.parse::<i64>().ok()) {
                m = m.assoc(kw("KeyCount"), Value::Long(kc));
            }
            m = m.assoc(
                kw("IsTruncated"),
                Value::Bool(wire::xml_tag(&xml, "IsTruncated").as_deref() == Some("true")),
            );
            if let Some(tok) = wire::xml_tag(&xml, "NextContinuationToken") {
                m = m.assoc(kw("NextContinuationToken"), Value::string(tok));
            }
            let prefixes: Vec<Value> = wire::xml_blocks(&xml, "CommonPrefixes")
                .into_iter()
                .filter_map(|b| wire::xml_tag(b, "Prefix"))
                .map(Value::string)
                .collect();
            if !prefixes.is_empty() {
                m = m.assoc(
                    kw("CommonPrefixes"),
                    Value::Vector(GcPtr::new(PersistentVector::from_iter(prefixes))),
                );
            }
        }
    }
    Value::Map(m)
}
