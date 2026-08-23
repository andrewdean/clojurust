//! Credential resolution — exactly the two chains causeway needs:
//!
//! 1. **Static keys**: explicit in the client config map, else the
//!    `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`/`AWS_SESSION_TOKEN` env.
//! 2. **IRSA** (EKS web identity): `AWS_WEB_IDENTITY_TOKEN_FILE` +
//!    `AWS_ROLE_ARN` exchanged via the unsigned
//!    `sts:AssumeRoleWithWebIdentity`, cached until near expiry.
//!
//! No profiles, SSO, IMDS, or explicit AssumeRole.

use std::time::{Duration, SystemTime};

/// Plain, `Send` credential material.
#[derive(Debug, Clone)]
pub struct Creds {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
    /// For cached web-identity creds.
    pub expires: Option<SystemTime>,
}

impl Creds {
    pub fn expired(&self) -> bool {
        match self.expires {
            // Refresh 2 minutes early.
            Some(t) => SystemTime::now() + Duration::from_secs(120) >= t,
            None => false,
        }
    }
}

/// Static portion of the chain (no network). `explicit` comes from the
/// client config map.
pub fn static_creds(explicit: Option<(String, String, Option<String>)>) -> Option<Creds> {
    if let Some((ak, sk, token)) = explicit {
        return Some(Creds {
            access_key_id: ak,
            secret_access_key: sk,
            session_token: token,
            expires: None,
        });
    }
    let ak = std::env::var("AWS_ACCESS_KEY_ID").ok()?;
    let sk = std::env::var("AWS_SECRET_ACCESS_KEY").ok()?;
    Some(Creds {
        access_key_id: ak,
        secret_access_key: sk,
        session_token: std::env::var("AWS_SESSION_TOKEN").ok(),
        expires: None,
    })
}

/// True when the IRSA env contract is present.
pub fn irsa_available() -> bool {
    std::env::var("AWS_WEB_IDENTITY_TOKEN_FILE").is_ok() && std::env::var("AWS_ROLE_ARN").is_ok()
}

/// Exchange the projected service-account token for temporary credentials.
/// Runs on the worker thread (blocking HTTP). The call is unsigned.
pub fn assume_role_with_web_identity(region: &str) -> Result<Creds, String> {
    let token_file = std::env::var("AWS_WEB_IDENTITY_TOKEN_FILE")
        .map_err(|_| "AWS_WEB_IDENTITY_TOKEN_FILE not set".to_string())?;
    let role_arn =
        std::env::var("AWS_ROLE_ARN").map_err(|_| "AWS_ROLE_ARN not set".to_string())?;
    let token = std::fs::read_to_string(&token_file)
        .map_err(|e| format!("reading {token_file}: {e}"))?;
    let session_name = std::env::var("AWS_ROLE_SESSION_NAME")
        .unwrap_or_else(|_| "cljrsh".to_string());

    let endpoint = format!("https://sts.{region}.amazonaws.com/");
    let params = [
        ("Action", "AssumeRoleWithWebIdentity"),
        ("Version", "2011-06-15"),
        ("RoleArn", role_arn.as_str()),
        ("RoleSessionName", session_name.as_str()),
        ("WebIdentityToken", token.trim()),
    ];
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(&endpoint)
        .form(&params)
        .send()
        .map_err(|e| format!("STS AssumeRoleWithWebIdentity: {e}"))?;
    let status = resp.status();
    let body = resp.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("STS AssumeRoleWithWebIdentity failed ({status}): {body}"));
    }
    let tag = |name: &str| -> Option<String> {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        let start = body.find(&open)? + open.len();
        let end = body[start..].find(&close)? + start;
        Some(body[start..end].trim().to_string())
    };
    let expires = tag("Expiration").and_then(|e| {
        cljrs_types_parse_rfc3339(&e).map(|ms| {
            SystemTime::UNIX_EPOCH + Duration::from_millis(ms as u64)
        })
    });
    Ok(Creds {
        access_key_id: tag("AccessKeyId").ok_or("STS response missing AccessKeyId")?,
        secret_access_key: tag("SecretAccessKey").ok_or("STS response missing SecretAccessKey")?,
        session_token: tag("SessionToken"),
        expires,
    })
}

fn cljrs_types_parse_rfc3339(s: &str) -> Option<i64> {
    // Reuse the runtime's dep-free parser through cljrsh-host's cljrs-types dep.
    cljrs_types::instant::parse_rfc3339_millis(s).ok()
}
