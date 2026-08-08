//! Rejects versioned resolution when host dependencies are not compiled.

use std::sync::Arc;

use crate::env::GlobalEnv;
use crate::error::{EvalError, EvalResult};

const VERSIONED_LOOKUP: &str = "versioned namespace lookup";

/// Strips a trailing version suffix from a namespace name.
pub fn base_ns_name(ns: &str) -> &str {
    cljrs_value::symbol::split_version(ns).0
}

/// Rejects a versioned value lookup without using host resources.
///
/// # Errors
///
/// Always returns `EvalError::ForbiddenEffect` in this build profile.
pub fn resolve_versioned_value(
    _globals: &Arc<GlobalEnv>,
    _defining_ns: &str,
    _ns_part: Option<&str>,
    _name: &str,
    _commit: &str,
) -> EvalResult {
    Err(forbidden())
}

/// Rejects versioned source discovery without using host resources.
///
/// # Errors
///
/// Always returns `EvalError::ForbiddenEffect` in this build profile.
pub fn pin_if_available(
    _globals: &Arc<GlobalEnv>,
    _base_ns: &str,
    _commit: &str,
) -> EvalResult<bool> {
    Err(forbidden())
}

/// Rejects versioned namespace loading without using host resources.
///
/// # Errors
///
/// Always returns `EvalError::ForbiddenEffect` in this build profile.
pub fn ensure_versioned_ns_loaded(
    _globals: &Arc<GlobalEnv>,
    _base_ns: &str,
    _commit: &str,
) -> EvalResult<Arc<str>> {
    Err(forbidden())
}

fn forbidden() -> EvalError {
    EvalError::ForbiddenEffect(VERSIONED_LOOKUP.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Env;
    use cljrs_reader::Form;
    use cljrs_value::{CljxFn, Value};

    fn eval_stub(_form: &Form, _env: &mut Env) -> EvalResult {
        Ok(Value::Nil)
    }

    fn call_stub(_function: &CljxFn, _arguments: &[Value], _env: &mut Env) -> EvalResult {
        Ok(Value::Nil)
    }

    #[test]
    fn restricted_profile_returns_a_stable_policy_error() {
        let globals = GlobalEnv::new(eval_stub, call_stub, None);
        let errors = [
            resolve_versioned_value(&globals, "user", None, "value", "abc1234")
                .expect_err("versioned values must be forbidden"),
            pin_if_available(&globals, "other.code", "abc1234")
                .expect_err("versioned discovery must be forbidden"),
            ensure_versioned_ns_loaded(&globals, "other.code", "abc1234")
                .expect_err("versioned loading must be forbidden"),
        ];

        assert!(errors.into_iter().all(
            |error| matches!(error, EvalError::ForbiddenEffect(operation) if operation == VERSIONED_LOOKUP)
        ));
    }
}
