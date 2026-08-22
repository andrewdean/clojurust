//! SigV4 signing + blocking HTTP execution on a worker thread, and the tiny
//! XML reader the S3/STS responses need. Everything crossing the thread
//! boundary is plain `Send` data (the `cljrsh.http` pattern).

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{
    SignableBody, SignableRequest, SignatureLocation, SigningSettings, sign,
};
use aws_sigv4::sign::v4;

use crate::creds::Creds;

/// A fully-planned HTTP exchange (all `Send`).
#[derive(Debug)]
pub struct Plan {
    pub method: String,
    /// Full URL including query string.
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// SigV4 service name ("s3", "secretsmanager", ...).
    pub service: String,
    pub region: String,
}

#[derive(Debug)]
pub struct Exchange {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

fn identity(creds: &Creds) -> aws_smithy_runtime_api::client::identity::Identity {
    Credentials::new(
        creds.access_key_id.clone(),
        creds.secret_access_key.clone(),
        creds.session_token.clone(),
        None,
        "cljrsh",
    )
    .into()
}

/// Sign `plan` (headers mode) and return the signed request pieces.
fn sign_plan(plan: &Plan, creds: &Creds) -> Result<http::Request<Vec<u8>>, String> {
    let mut request = http::Request::builder()
        .method(plan.method.as_str())
        .uri(&plan.url);
    for (k, v) in &plan.headers {
        request = request.header(k, v);
    }
    let mut request = request
        .body(plan.body.clone())
        .map_err(|e| format!("building request: {e}"))?;

    let ident = identity(creds);
    let mut settings = SigningSettings::default();
    // S3 requires the payload hash header and unnormalized paths.
    if plan.service == "s3" {
        settings.payload_checksum_kind = aws_sigv4::http_request::PayloadChecksumKind::XAmzSha256;
        settings.uri_path_normalization_mode =
            aws_sigv4::http_request::UriPathNormalizationMode::Disabled;
    }
    let params = v4::SigningParams::builder()
        .identity(&ident)
        .region(&plan.region)
        .name(&plan.service)
        .time(SystemTime::now())
        .settings(settings)
        .build()
        .map_err(|e| format!("signing params: {e}"))?;
    let header_pairs: Vec<(&str, &str)> = request
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str(), v.to_str().unwrap_or("")))
        .collect();
    let signable = SignableRequest::new(
        plan.method.as_str(),
        plan.url.as_str(),
        header_pairs.into_iter(),
        SignableBody::Bytes(&plan.body),
    )
    .map_err(|e| format!("signable request: {e}"))?;
    let out = sign(signable, &params.into()).map_err(|e| format!("sigv4: {e}"))?;
    out.into_parts().0.apply_to_request_http1x(&mut request);
    Ok(request)
}

/// Sign + send on a dedicated thread (reqwest's blocking runtime must not be
/// created inside the interpreter's LocalSet drive).
pub fn execute(plan: Plan, creds: Creds) -> Result<Exchange, String> {
    std::thread::Builder::new()
        .name("cljrsh-aws".into())
        .spawn(move || -> Result<Exchange, String> {
            let signed = sign_plan(&plan, &creds)?;
            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .map_err(|e| e.to_string())?;
            let mut req = client.request(
                reqwest::Method::from_bytes(plan.method.as_bytes())
                    .map_err(|e| e.to_string())?,
                signed.uri().to_string(),
            );
            for (k, v) in signed.headers() {
                if k.as_str() != "host" {
                    req = req.header(k.as_str(), v.to_str().unwrap_or(""));
                }
            }
            let resp = req
                .body(plan.body)
                .send()
                .map_err(|e| format!("request to {} failed: {e}", plan.url))?;
            let status = resp.status().as_u16();
            let headers = resp
                .headers()
                .iter()
                .map(|(k, v)| {
                    (
                        k.as_str().to_string(),
                        String::from_utf8_lossy(v.as_bytes()).into_owned(),
                    )
                })
                .collect();
            let body = resp.bytes().map_err(|e| e.to_string())?.to_vec();
            Ok(Exchange {
                status,
                headers,
                body,
            })
        })
        .map_err(|e| format!("spawning aws thread: {e}"))?
        .join()
        .map_err(|_| "aws worker thread panicked".to_string())?
}

/// Presign a GET: query-param signature with an expiry; returns the URL.
pub fn presign(plan: &Plan, creds: &Creds, expires_in: Duration) -> Result<String, String> {
    let ident = identity(creds);
    let mut settings = SigningSettings::default();
    settings.signature_location = SignatureLocation::QueryParams;
    settings.expires_in = Some(expires_in);
    if plan.service == "s3" {
        settings.uri_path_normalization_mode =
            aws_sigv4::http_request::UriPathNormalizationMode::Disabled;
    }
    let params = v4::SigningParams::builder()
        .identity(&ident)
        .region(&plan.region)
        .name(&plan.service)
        .time(SystemTime::now())
        .settings(settings)
        .build()
        .map_err(|e| format!("signing params: {e}"))?;
    let signable = SignableRequest::new(
        plan.method.as_str(),
        plan.url.as_str(),
        std::iter::empty(),
        SignableBody::UnsignedPayload,
    )
    .map_err(|e| format!("signable request: {e}"))?;
    let out = sign(signable, &params.into()).map_err(|e| format!("sigv4: {e}"))?;
    let mut request = http::Request::builder()
        .method(plan.method.as_str())
        .uri(&plan.url)
        .body(())
        .map_err(|e| e.to_string())?;
    out.into_parts().0.apply_to_request_http1x(&mut request);
    Ok(request.uri().to_string())
}

// ── Minimal XML reading (S3 list/error and STS responses) ────────────────────

/// First text content of `<tag>...</tag>` inside `xml`.
pub fn xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml_unescape(xml[start..end].trim()))
}

/// Every `<block>...</block>` body, in order.
pub fn xml_blocks<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let inner = start + open.len();
        let Some(end) = rest[inner..].find(&close) else {
            break;
        };
        out.push(&rest[inner..inner + end]);
        rest = &rest[inner + end + close.len()..];
    }
    out
}

pub fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Percent-encode one URI path segment / query value (S3 canonical style).
pub fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            b'/' if !encode_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Assemble a URL from endpoint + path + query pairs.
pub fn build_url(endpoint: &str, path: &str, query: &BTreeMap<String, String>) -> String {
    let mut url = format!("{}{}", endpoint.trim_end_matches('/'), path);
    if !query.is_empty() {
        let qs: Vec<String> = query
            .iter()
            .map(|(k, v)| {
                if v.is_empty() {
                    uri_encode(k, true)
                } else {
                    format!("{}={}", uri_encode(k, true), uri_encode(v, true))
                }
            })
            .collect();
        url.push('?');
        url.push_str(&qs.join("&"));
    }
    url
}
