//! `cljrsh.term` — terminal and small networking probes backing
//! `babashka.terminal` and `babashka.wait`.

use std::io::IsTerminal;

use cljrs_interop::{Registry, wrap_fn_variadic};
use cljrs_value::Value;

pub fn register(registry: &mut Registry) {
    registry.define(
        "cljrsh.term/tty?",
        wrap_fn_variadic(
            "cljrsh.term/tty?",
            0,
            |args: &[Value]| -> Result<Value, String> {
                let which = match args.first() {
                    None => "stdout".to_string(),
                    Some(Value::Keyword(k)) => k.get().name.to_string(),
                    Some(other) => {
                        return Err(format!(
                            "tty? expects :stdin/:stdout/:stderr, got {}",
                            other.type_name()
                        ));
                    }
                };
                let is_tty = match which.as_str() {
                    "stdin" => std::io::stdin().is_terminal(),
                    "stdout" => std::io::stdout().is_terminal(),
                    "stderr" => std::io::stderr().is_terminal(),
                    other => {
                        return Err(format!("tty? expects :stdin/:stdout/:stderr, got :{other}"));
                    }
                };
                Ok(Value::Bool(is_tty))
            },
        ),
    );

    // (tcp-open? host port timeout-ms) — one connection attempt.
    registry.define(
        "cljrsh.term/tcp-open?",
        wrap_fn_variadic(
            "cljrsh.term/tcp-open?",
            3,
            |args: &[Value]| -> Result<Value, String> {
                let Value::Str(host) = &args[0] else {
                    return Err("host must be a string".to_string());
                };
                let Value::Long(port) = &args[1] else {
                    return Err("port must be an integer".to_string());
                };
                let Value::Long(timeout_ms) = &args[2] else {
                    return Err("timeout-ms must be an integer".to_string());
                };
                use std::net::ToSocketAddrs;
                let addr = format!("{}:{port}", host.get());
                let Ok(mut addrs) = addr.to_socket_addrs() else {
                    return Ok(Value::Bool(false));
                };
                let Some(addr) = addrs.next() else {
                    return Ok(Value::Bool(false));
                };
                let open = std::net::TcpStream::connect_timeout(
                    &addr,
                    std::time::Duration::from_millis(*timeout_ms as u64),
                )
                .is_ok();
                Ok(Value::Bool(open))
            },
        ),
    );

    registry.define(
        "cljrsh.term/sleep-ms",
        wrap_fn_variadic(
            "cljrsh.term/sleep-ms",
            1,
            |args: &[Value]| -> Result<Value, String> {
                let Value::Long(ms) = &args[0] else {
                    return Err("sleep-ms expects an integer".to_string());
                };
                std::thread::sleep(std::time::Duration::from_millis(*ms as u64));
                Ok(Value::Nil)
            },
        ),
    );

    registry.define(
        "cljrsh.term/now-millis",
        wrap_fn_variadic(
            "cljrsh.term/now-millis",
            0,
            |_args: &[Value]| -> Result<Value, String> {
                Ok(Value::Long(
                    std::time::SystemTime::now()
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0),
                ))
            },
        ),
    );
}
