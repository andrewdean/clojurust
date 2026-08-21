use cljrs_value::{Value, ValueError, ValueResult};
use num_traits::ToPrimitive;
use std::time::SystemTime;

pub(crate) fn builtin_nanotime(_args: &[Value]) -> ValueResult<Value> {
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(nanos) => Ok(Value::Long(
            nanos
                .as_nanos()
                .to_i64()
                .ok_or_else(|| ValueError::OutOfRange)?,
        )),
        Err(e) => Err(ValueError::Other(format!("{}", e))),
    }
}

/// `(instant-now)` — the current time as a `#inst` value.
pub(crate) fn builtin_instant_now(_args: &[Value]) -> ValueResult<Value> {
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => Ok(Value::Instant(
            d.as_millis().to_i64().ok_or(ValueError::OutOfRange)?,
        )),
        Err(e) => Err(ValueError::Other(format!("{e}"))),
    }
}

/// `(instant ms-or-string)` — build a `#inst` from epoch millis or an
/// RFC 3339 string.
pub(crate) fn builtin_instant(args: &[Value]) -> ValueResult<Value> {
    match &args[0] {
        Value::Long(ms) => Ok(Value::Instant(*ms)),
        Value::Instant(ms) => Ok(Value::Instant(*ms)),
        Value::Str(s) => cljrs_types::instant::parse_rfc3339_millis(s.get())
            .map(Value::Instant)
            .map_err(ValueError::Other),
        other => Err(ValueError::WrongType {
            expected: "integer millis or RFC 3339 string",
            got: other.type_name().to_string(),
        }),
    }
}

/// `(instant-ms inst)` — epoch milliseconds of a `#inst` value.
pub(crate) fn builtin_instant_ms(args: &[Value]) -> ValueResult<Value> {
    match &args[0] {
        Value::Instant(ms) => Ok(Value::Long(*ms)),
        other => Err(ValueError::WrongType {
            expected: "instant",
            got: other.type_name().to_string(),
        }),
    }
}

/// `(instant? x)`.
pub(crate) fn builtin_instant_q(args: &[Value]) -> ValueResult<Value> {
    Ok(Value::Bool(matches!(&args[0], Value::Instant(_))))
}
