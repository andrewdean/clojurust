//! `cljrsh.hash` — content hashing for scripts (the shasum/sha256sum idiom).

use cljrs_interop::{Registry, wrap_fn_variadic};
use cljrs_value::Value;
use sha2::{Digest, Sha256};

fn to_bytes(v: &Value) -> Result<Vec<u8>, String> {
    match v {
        Value::Str(s) => Ok(s.get().as_bytes().to_vec()),
        Value::ByteArray(b) => Ok(b
            .get()
            .lock()
            .unwrap()
            .iter()
            .map(|&x| x as u8)
            .collect()),
        other => Err(format!(
            "sha256 expects a string or byte array, got {}",
            other.type_name()
        )),
    }
}

pub fn register(registry: &mut Registry) {
    registry.define(
        "cljrsh.hash/sha256-hex",
        wrap_fn_variadic(
            "cljrsh.hash/sha256-hex",
            1,
            |args: &[Value]| -> Result<Value, String> {
                let bytes = to_bytes(&args[0])?;
                let digest = Sha256::digest(&bytes);
                Ok(Value::string(hex::encode(digest)))
            },
        ),
    );
    registry.define(
        "cljrsh.hash/sha256-file-hex",
        wrap_fn_variadic(
            "cljrsh.hash/sha256-file-hex",
            1,
            |args: &[Value]| -> Result<Value, String> {
                let Value::Str(path) = &args[0] else {
                    return Err("sha256-file-hex expects a path string".into());
                };
                let bytes = std::fs::read(path.get().as_str())
                    .map_err(|e| format!("sha256-file-hex {}: {e}", path.get()))?;
                let digest = Sha256::digest(&bytes);
                Ok(Value::string(hex::encode(digest)))
            },
        ),
    );
}
