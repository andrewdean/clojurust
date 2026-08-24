//! A minimal babashka pod used by cljrsh-pods' tests, mirroring babashka's
//! reference `test-resources/pod.clj`: bencode over stdio, EDN payloads.
//!
//! Exposes `pod.test-pod/add-sync` (sums numeric args), `error-fn` (always
//! errors), `print-fn` (writes to the client's stdout via an "out" reply),
//! and a `"code"` var `from-code` evaluated client-side.

use std::collections::BTreeMap;
use std::io::{Read, Write};

use cljrs_bencode::{Bencode, decode, encode_to_vec};

fn bstr(s: &str) -> Bencode {
    Bencode::str(s)
}

fn dict(entries: Vec<(&str, Bencode)>) -> Bencode {
    let mut m = BTreeMap::new();
    for (k, v) in entries {
        m.insert(k.as_bytes().to_vec(), v);
    }
    Bencode::Dict(m)
}

fn get<'a>(d: &'a Bencode, key: &str) -> Option<&'a Bencode> {
    d.as_dict()?.get(key.as_bytes())
}

fn send(msg: &Bencode) {
    let bytes = encode_to_vec(msg);
    let mut out = std::io::stdout().lock();
    out.write_all(&bytes).unwrap();
    out.flush().unwrap();
}

fn main() {
    assert_eq!(
        std::env::var("BABASHKA_POD").as_deref(),
        Ok("true"),
        "pods are only started by a pod client"
    );
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut stdin = std::io::stdin().lock();
    loop {
        match decode(&buf) {
            Ok(Some((msg, used))) => {
                buf.drain(..used);
                if !handle(&msg) {
                    return;
                }
            }
            Ok(None) => {
                let n = stdin.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    return; // client went away
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Err(e) => {
                eprintln!("test-pod: bad bencode: {e}");
                return;
            }
        }
    }
}

/// Handle one message; false = shut down.
fn handle(msg: &Bencode) -> bool {
    let op = get(msg, "op").and_then(|v| v.as_str()).unwrap_or("");
    match op {
        "describe" => {
            send(&dict(vec![
                ("format", bstr("edn")),
                (
                    "namespaces",
                    Bencode::List(vec![dict(vec![
                        ("name", bstr("pod.test-pod")),
                        (
                            "vars",
                            Bencode::List(vec![
                                dict(vec![("name", bstr("add-sync"))]),
                                dict(vec![("name", bstr("error-fn"))]),
                                dict(vec![("name", bstr("print-fn"))]),
                                dict(vec![
                                    ("name", bstr("from-code")),
                                    ("code", bstr("(defn from-code [] :evaluated-client-side)")),
                                ]),
                            ]),
                        ),
                    ])]),
                ),
                ("ops", dict(vec![("shutdown", dict(vec![]))])),
            ]));
            true
        }
        "invoke" => {
            let id = get(msg, "id").and_then(|v| v.as_str()).unwrap_or("");
            let var = get(msg, "var").and_then(|v| v.as_str()).unwrap_or("");
            let args = get(msg, "args").and_then(|v| v.as_str()).unwrap_or("[]");
            match var {
                "pod.test-pod/add-sync" => {
                    // args is an EDN vector of numbers, e.g. "[1 2 3]".
                    let sum: i64 = args
                        .trim_matches(['[', ']'])
                        .split_whitespace()
                        .filter_map(|t| t.parse::<i64>().ok())
                        .sum();
                    send(&dict(vec![
                        ("id", bstr(id)),
                        ("value", bstr(&sum.to_string())),
                        ("status", Bencode::List(vec![bstr("done")])),
                    ]));
                }
                "pod.test-pod/print-fn" => {
                    send(&dict(vec![
                        ("id", bstr(id)),
                        ("out", bstr("hello from pod\n")),
                    ]));
                    send(&dict(vec![
                        ("id", bstr(id)),
                        ("value", bstr(":printed")),
                        ("status", Bencode::List(vec![bstr("done")])),
                    ]));
                }
                "pod.test-pod/error-fn" => {
                    send(&dict(vec![
                        ("id", bstr(id)),
                        ("ex-message", bstr("pod exploded")),
                        ("ex-data", bstr("{:pod-var :error-fn}")),
                        ("status", Bencode::List(vec![bstr("done"), bstr("error")])),
                    ]));
                }
                other => {
                    send(&dict(vec![
                        ("id", bstr(id)),
                        ("ex-message", bstr(&format!("no such var: {other}"))),
                        ("ex-data", bstr("{}")),
                        ("status", Bencode::List(vec![bstr("done"), bstr("error")])),
                    ]));
                }
            }
            true
        }
        "shutdown" => false,
        other => {
            eprintln!("test-pod: unknown op {other:?}");
            true
        }
    }
}
