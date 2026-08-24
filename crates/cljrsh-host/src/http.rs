//! `cljrsh.http` — blocking HTTP client over reqwest (rustls).
//!
//! One native entry point, `(request opts)`, mirroring babashka.http-client's
//! request map; the convenience verbs and `:throw` behavior live in the
//! `babashka.http-client` veneer.
//!
//! The interpreter thread sits inside a Tokio LocalSet drive, and reqwest's
//! blocking client owns a private runtime that must never be created or
//! dropped inside an async context. So the options map is parsed into plain
//! data first, the entire client lifecycle runs on a dedicated thread, and
//! only `Send` primitives cross back.

use std::time::Duration;

use cljrs_interop::{Registry, wrap_fn1};
use cljrs_value::value::MapValue;
use cljrs_value::{Keyword, Value};

fn kw(name: &str) -> Value {
    Value::keyword(Keyword::simple(name))
}

fn get_opt(m: &MapValue, name: &str) -> Option<Value> {
    m.get(&kw(name))
}

fn as_str(v: &Value, what: &str) -> Result<String, String> {
    match v {
        Value::Str(s) => Ok(s.get().to_string()),
        Value::Keyword(k) => Ok(k.get().name.to_string()),
        other => Err(format!(
            "{what} must be a string, got {}",
            other.type_name()
        )),
    }
}

fn string_pairs(v: &Value, what: &str) -> Result<Vec<(String, String)>, String> {
    let Value::Map(m) = v else {
        return Err(format!("{what} must be a map, got {}", v.type_name()));
    };
    let mut out = Vec::new();
    for (k, val) in m.iter() {
        out.push((as_str(k, what)?, as_str(val, what)?));
    }
    Ok(out)
}

/// Fully-parsed request: plain `Send` data, no interpreter types.
struct Plan {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    query: Vec<(String, String)>,
    form: Vec<(String, String)>,
    basic_auth: Option<(String, String)>,
    body: Option<String>,
    timeout_ms: Option<u64>,
    follow_redirects: bool,
}

fn parse_plan(opts: &Value) -> Result<Plan, String> {
    let Value::Map(m) = opts else {
        return Err(format!(
            "request expects an options map, got {}",
            opts.type_name()
        ));
    };
    let url = get_opt(m, "url")
        .ok_or_else(|| "request map needs :url".to_string())
        .and_then(|v| as_str(&v, ":url"))?;
    let method = match get_opt(m, "method") {
        Some(v) => as_str(&v, ":method")?.to_uppercase(),
        None => "GET".to_string(),
    };
    let timeout_ms = match get_opt(m, "timeout-ms") {
        Some(Value::Long(ms)) => Some(ms as u64),
        Some(_) => return Err(":timeout-ms must be an integer".to_string()),
        None => None,
    };
    let follow_redirects = !matches!(get_opt(m, "follow-redirects"), Some(Value::Bool(false)));
    let headers = match get_opt(m, "headers") {
        Some(v) => string_pairs(&v, ":headers")?,
        None => Vec::new(),
    };
    let query = match get_opt(m, "query-params") {
        Some(v) => string_pairs(&v, ":query-params")?,
        None => Vec::new(),
    };
    let form = match get_opt(m, "form-params") {
        Some(v) => string_pairs(&v, ":form-params")?,
        None => Vec::new(),
    };
    let basic_auth = match get_opt(m, "basic-auth") {
        Some(Value::Vector(pair)) if pair.get().count() == 2 => Some((
            as_str(pair.get().nth(0).unwrap(), ":basic-auth user")?,
            as_str(pair.get().nth(1).unwrap(), ":basic-auth pass")?,
        )),
        Some(other) => {
            return Err(format!(
                ":basic-auth must be [user pass], got {}",
                other.type_name()
            ));
        }
        None => None,
    };
    let body = match get_opt(m, "body") {
        Some(v) => Some(as_str(&v, ":body")?),
        None => None,
    };
    Ok(Plan {
        method,
        url,
        headers,
        query,
        form,
        basic_auth,
        body,
        timeout_ms,
        follow_redirects,
    })
}

type Exchange = (i64, Vec<(String, String)>, String);

/// Build the client, send, and read the body — on the current (worker) thread.
fn execute(plan: Plan) -> Result<Exchange, String> {
    let method = reqwest::Method::from_bytes(plan.method.as_bytes())
        .map_err(|_| format!("invalid :method {:?}", plan.method))?;
    let mut client = reqwest::blocking::Client::builder();
    if let Some(ms) = plan.timeout_ms {
        client = client.timeout(Duration::from_millis(ms));
    }
    if !plan.follow_redirects {
        client = client.redirect(reqwest::redirect::Policy::none());
    }
    let client = client
        .build()
        .map_err(|e| format!("http client build failed: {e}"))?;

    let mut req = client.request(method, &plan.url);
    for (k, v) in &plan.headers {
        req = req.header(k, v);
    }
    if !plan.query.is_empty() {
        req = req.query(&plan.query);
    }
    if !plan.form.is_empty() {
        req = req.form(&plan.form);
    }
    if let Some((user, pass)) = &plan.basic_auth {
        req = req.basic_auth(user, Some(pass));
    }
    if let Some(body) = plan.body {
        req = req.body(body);
    }

    let resp = req.send().map_err(|e| format!("request failed: {e}"))?;
    let status = resp.status().as_u16() as i64;
    let headers = resp
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    let body = resp
        .text()
        .map_err(|e| format!("reading response body failed: {e}"))?;
    Ok((status, headers, body))
}

pub fn register(registry: &mut Registry) {
    registry.define(
        "cljrsh.http/request*",
        wrap_fn1(
            "cljrsh.http/request*",
            |opts: Value| -> Result<Value, String> {
                let plan = parse_plan(&opts)?;
                let url = plan.url.clone();
                let (status, header_pairs, body) = std::thread::Builder::new()
                    .name("cljrsh-http".into())
                    .spawn(move || execute(plan))
                    .map_err(|e| format!("failed to spawn http thread: {e}"))?
                    .join()
                    .map_err(|_| format!("http thread panicked for {url}"))?
                    .map_err(|e| format!("request to {url}: {e}"))?;

                let mut headers = MapValue::empty();
                for (name, value) in header_pairs {
                    headers = headers.assoc(Value::string(name), Value::string(value));
                }
                let mut out = MapValue::empty();
                out = out.assoc(kw("status"), Value::Long(status));
                out = out.assoc(kw("headers"), Value::Map(headers));
                out = out.assoc(kw("body"), Value::string(body));
                Ok(Value::Map(out))
            },
        ),
    );
}
