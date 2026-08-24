//! C ABI for embedding the cljrs runtime in a Qt/QML host (rung 3 of the
//! clj.rs → QuickShell ladder).
//!
//! The cljrs runtime is single-threaded per `GlobalEnv` — GC'd values never
//! cross threads — so the handle owns a dedicated interpreter thread, and
//! that thread *is* the nREPL serve loop (`cljrs-nrepl`'s network thread
//! feeds it jobs). Everything evaluates there:
//!
//! - **QML evals** ride the same protocol: `cljrs_qt_eval` is a localhost
//!   nREPL client speaking bencode over a persistent connection. One loop,
//!   no upstream changes, and CIDER connects to the very same environment
//!   the shell widget uses — defs made from the editor are live in QML.
//! - **State push**: the runtime interns `(qml/set! :key value)`; every call
//!   reaches the registered state callback with the key and the value as
//!   JSON. It fires on the interpreter thread — the C++ side must marshal
//!   to the QML thread.
//!
//! Results cross the eval boundary as a malloc'd JSON envelope
//! (`{"ok": ...}` / `{"error": ...}`). nREPL returns printed values, so the
//! envelope re-derives scalars (nil/bool/int/float/string) and carries
//! anything else as its printed form.
//!
//! The nREPL port binds 127.0.0.1 only, but is always on: anything that can
//! reach localhost can eval in the embedding process. Same trust model as a
//! user-session `cljrs nrepl`, worth knowing when the host is a shell.

// C-ABI entry points take raw pointers whose validity is the caller's
// contract; each deref site carries its own `unsafe` block.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cljrs_bencode::Bencode;
use cljrs_gc::GcPtr;
use cljrs_value::{Arity, NativeFn, Value};

/// `extern "C"` callback invoked for every `(qml/set! key value)`.
/// `key` is the bare name (no leading colon); `value_json` is the same JSON
/// encoding eval results use. Both pointers are valid only for the call, and
/// it fires on the interpreter thread.
pub type StateCallback =
    extern "C" fn(user: *mut c_void, key: *const c_char, value_json: *const c_char);

type CallbackSlot = Arc<Mutex<Option<(StateCallback, usize)>>>;

pub struct Runtime {
    port: u16,
    conn: Mutex<Option<TcpStream>>,
    msg_id: AtomicU64,
    state_cb: CallbackSlot,
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn value_json(v: &Value) -> String {
    match v {
        Value::Nil => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Long(n) => n.to_string(),
        Value::Double(d) if d.is_finite() => d.to_string(),
        Value::Str(s) => format!("\"{}\"", json_escape(s.get())),
        other => format!("\"{}\"", json_escape(&other.to_string())),
    }
}

/// Map an nREPL *printed* value back to a JSON value: scalars become native
/// JSON, everything else the printed text as a JSON string.
fn printed_to_json(printed: &str) -> String {
    match printed {
        "nil" => return "null".to_string(),
        "true" => return "true".to_string(),
        "false" => return "false".to_string(),
        _ => {}
    }
    if printed.parse::<i64>().is_ok() {
        return printed.to_string();
    }
    if let Ok(f) = printed.parse::<f64>()
        && f.is_finite()
        && printed.chars().all(|c| "+-.eE0123456789".contains(c))
    {
        return printed.to_string();
    }
    if printed.len() >= 2 && printed.starts_with('"') && printed.ends_with('"') {
        // pr-str escapes only backslash and quote; undo, then JSON-escape.
        let inner = &printed[1..printed.len() - 1];
        let unescaped = inner.replace("\\\"", "\"").replace("\\\\", "\\");
        return format!("\"{}\"", json_escape(&unescaped));
    }
    format!("\"{}\"", json_escape(printed))
}

/// Intern the `qml` namespace: `(qml/set! key value)` pushes through the
/// state callback. Key may be a keyword, string, or symbol; the printed
/// form minus any leading colon is what the host sees.
fn register_qml_ns(globals: &Arc<cljrs_eval::GlobalEnv>, slot: CallbackSlot) {
    let func = move |args: &[Value]| -> cljrs_value::ValueResult<Value> {
        let key = match &args[0] {
            Value::Str(s) => s.get().clone(),
            other => other.to_string().trim_start_matches(':').to_string(),
        };
        let vjson = value_json(&args[1]);
        if let Some((cb, user)) = *slot.lock().unwrap()
            && let (Ok(ck), Ok(cv)) = (
                CString::new(key.replace('\0', "")),
                CString::new(vjson.replace('\0', "")),
            )
        {
            cb(user as *mut c_void, ck.as_ptr(), cv.as_ptr());
        }
        Ok(Value::Nil)
    };
    globals.intern(
        "qml",
        Arc::from("set!"),
        Value::NativeFunction(GcPtr::new(NativeFn {
            name: "qml/set!".into(),
            arity: Arity::Fixed(2),
            func: Arc::new(func),
        })),
    );
    globals.mark_loaded("qml");
}

/// Create a runtime: spawn the interpreter thread, which builds the
/// environment and serves nREPL on a 127.0.0.1 OS-assigned port for the
/// life of the process. Returns null if startup fails.
#[unsafe(no_mangle)]
pub extern "C" fn cljrs_qt_new() -> *mut Runtime {
    let slot: CallbackSlot = Arc::new(Mutex::new(None));
    let thread_slot = slot.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("cljrs-interp".into())
        .spawn(move || {
            let globals = cljrs_stdlib::standard_env();
            register_qml_ns(&globals, thread_slot);
            let config = cljrs_nrepl::Config {
                addr: ([127, 0, 0, 1], 0).into(),
                port_file: None,
            };
            match cljrs_nrepl::start(config, globals) {
                Ok(server) => {
                    let _ = tx.send(server.port());
                    let _ = server.serve();
                }
                Err(_) => {
                    let _ = tx.send(0);
                }
            }
        });
    if spawned.is_err() {
        return std::ptr::null_mut();
    }
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(port) if port > 0 => Box::into_raw(Box::new(Runtime {
            port,
            conn: Mutex::new(None),
            msg_id: AtomicU64::new(1),
            state_cb: slot,
        })),
        _ => std::ptr::null_mut(),
    }
}

/// The port the embedded nREPL server is listening on (127.0.0.1).
#[unsafe(no_mangle)]
pub extern "C" fn cljrs_qt_nrepl_port(rt: *const Runtime) -> u16 {
    if rt.is_null() {
        0
    } else {
        unsafe { &*rt }.port
    }
}

/// Register (or clear, with null) the state callback for `qml/set!`.
#[unsafe(no_mangle)]
pub extern "C" fn cljrs_qt_set_state_callback(
    rt: *mut Runtime,
    cb: Option<StateCallback>,
    user: *mut c_void,
) {
    if rt.is_null() {
        return;
    }
    let rt = unsafe { &mut *rt };
    *rt.state_cb.lock().unwrap() = cb.map(|f| (f, user as usize));
}

fn bstr(map: &BTreeMap<Vec<u8>, Bencode>, key: &str) -> Option<String> {
    map.get(key.as_bytes())
        .and_then(|v| v.as_str())
        .map(String::from)
}

fn eval_over_nrepl(rt: &Runtime, code: &str) -> Result<String, String> {
    let mut guard = rt
        .conn
        .lock()
        .map_err(|_| "connection poisoned".to_string())?;
    if guard.is_none() {
        let stream = TcpStream::connect(("127.0.0.1", rt.port))
            .map_err(|e| format!("nrepl connect failed: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(120)))
            .map_err(|e| e.to_string())?;
        *guard = Some(stream);
    }
    let stream = guard.as_mut().expect("connection set above");

    let id = rt.msg_id.fetch_add(1, Ordering::Relaxed).to_string();
    let mut msg = BTreeMap::new();
    msg.insert(b"op".to_vec(), Bencode::str("eval"));
    msg.insert(b"code".to_vec(), Bencode::str(code));
    msg.insert(b"id".to_vec(), Bencode::str(&id));
    let out = cljrs_bencode::encode_to_vec(&Bencode::Dict(msg));

    let mut run = || -> Result<String, String> {
        stream
            .write_all(&out)
            .map_err(|e| format!("nrepl write failed: {e}"))?;

        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        let mut value: Option<String> = None;
        let mut err_text = String::new();
        loop {
            while let Some((frame, used)) =
                cljrs_bencode::decode(&buf).map_err(|e| format!("bencode: {e}"))?
            {
                buf.drain(..used);
                let Some(dict) = frame.as_dict() else {
                    continue;
                };
                // Single client on this connection, but stay strict anyway.
                if bstr(dict, "id").as_deref() != Some(id.as_str()) {
                    continue;
                }
                if let Some(v) = bstr(dict, "value") {
                    value = Some(v);
                }
                if let Some(e) = bstr(dict, "err") {
                    err_text.push_str(&e);
                }
                if let Some(e) = bstr(dict, "ex") {
                    if !err_text.is_empty() {
                        err_text.push(' ');
                    }
                    err_text.push_str(&e);
                }
                let done = dict
                    .get(b"status".as_slice())
                    .and_then(|s| match s {
                        Bencode::List(items) => Some(items),
                        _ => None,
                    })
                    .map(|items| items.iter().any(|i| i.as_str() == Some("done")))
                    .unwrap_or(false);
                if done {
                    return if err_text.is_empty() {
                        Ok(value.unwrap_or_else(|| "nil".to_string()))
                    } else {
                        Err(err_text)
                    };
                }
            }
            let n = stream
                .read(&mut chunk)
                .map_err(|e| format!("nrepl read failed: {e}"))?;
            if n == 0 {
                return Err("nrepl connection closed".to_string());
            }
            buf.extend_from_slice(&chunk[..n]);
        }
    };

    let result = run();
    if result.is_err() {
        // A failed exchange leaves unknown bytes in flight; reconnect next call.
        *guard = None;
    }
    result
}

/// Evaluate all forms in `code` on the interpreter thread; return
/// `{"ok": <last value>}` or `{"error": "..."}` as a malloc'd C string.
/// Free with `cljrs_qt_free_str`. Blocks the calling thread for the
/// duration of the eval.
#[unsafe(no_mangle)]
pub extern "C" fn cljrs_qt_eval(rt: *mut Runtime, code: *const c_char) -> *mut c_char {
    let out: String = (|| {
        if rt.is_null() || code.is_null() {
            return r#"{"error":"null argument"}"#.to_string();
        }
        let rt = unsafe { &*rt };
        let code = match unsafe { CStr::from_ptr(code) }.to_str() {
            Ok(s) => s,
            Err(_) => return r#"{"error":"code is not valid utf-8"}"#.to_string(),
        };
        match eval_over_nrepl(rt, code) {
            Ok(printed) => format!("{{\"ok\":{}}}", printed_to_json(&printed)),
            Err(e) => format!("{{\"error\":\"{}\"}}", json_escape(&e)),
        }
    })();
    CString::new(out.replace('\0', ""))
        .expect("NUL stripped above")
        .into_raw()
}

/// Free a string returned by `cljrs_qt_eval`.
#[unsafe(no_mangle)]
pub extern "C" fn cljrs_qt_free_str(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}

/// Destroy a runtime handle. The interpreter thread and nREPL server keep
/// running for the life of the process — GC'd state cannot be torn down
/// from this thread; process exit reclaims everything.
#[unsafe(no_mangle)]
pub extern "C" fn cljrs_qt_destroy(rt: *mut Runtime) {
    if !rt.is_null() {
        drop(unsafe { Box::from_raw(rt) });
    }
}
