//! Environment bootstrap and program execution: shebang handling, preloads,
//! evaluation on the shared async driver, and exit-code mapping.

use std::sync::Arc;

use cljrs_env::env::{Env, GlobalEnv};
use cljrs_env::error::EvalError;
use cljrs_value::{Keyword, Value};

/// Build the full cljrsh environment: clojurust stdlib + async/io/charset +
/// host namespaces, babashka-flavored reader features, and command-line args.
pub fn setup_globals(extra_paths: Vec<std::path::PathBuf>, args: &[String]) -> Arc<GlobalEnv> {
    // Widen reader conditionals before any source is read: bb-flavored .cljc
    // picks its :bb branch, cljrsh-specific code its :cljrsh branch.
    cljrs_builtins::form::set_reader_features(["bb", "cljrsh", "rust"]);

    let globals = cljrs_stdlib::standard_env_with_paths_and_config(
        extra_paths,
        Arc::new(cljrs_gc::GcConfig::new()),
    );

    // Async runtime hookups run inside the LocalSet (init spawns tasks).
    crate::with_driver(|drv| {
        let init = |g: &Arc<GlobalEnv>| {
            cljrs_async::init(g);
            cljrs_io::init(g);
            cljrs_charset::init(g);
        };
        match drv {
            Some((rt, local)) => local.block_on(rt, async { init(&globals) }),
            None => init(&globals),
        }
    });
    cljrs_process::init(&globals);
    cljrsh_host::init(&globals);

    cljrs_builtins::system::set_command_line_args(&globals, args);
    globals
}

/// Evaluate `src` as a program: strip a shebang line, run preloads first,
/// evaluate every form, and map the outcome to a process exit code.
///
/// `print_result` prints the final non-nil value (the `-e` behavior).
pub fn run_program(
    globals: &Arc<GlobalEnv>,
    src: &str,
    filename: &str,
    print_result: bool,
) -> i32 {
    let mut env = Env::new(globals.clone(), "user");

    if let Some(preloads) = preloads() {
        match eval_str(&mut env, &preloads, "<preloads>") {
            Ok(_) => {}
            Err(e) => return report_error(e),
        }
    }

    let src = strip_shebang(src);
    match eval_str(&mut env, src, filename) {
        Ok(value) => {
            if print_result && value != Value::Nil {
                println!("{value}");
            }
            0
        }
        Err(e) => report_error(e),
    }
}

/// `CLJRSH_PRELOADS` wins over `BABASHKA_PRELOADS`.
fn preloads() -> Option<String> {
    std::env::var("CLJRSH_PRELOADS")
        .ok()
        .or_else(|| std::env::var("BABASHKA_PRELOADS").ok())
        .filter(|s| !s.trim().is_empty())
}

/// Replace a leading `#!` line with a bare newline so line numbers in errors
/// still match the file.
pub fn strip_shebang(src: &str) -> &str {
    if src.starts_with("#!") {
        match src.find('\n') {
            Some(idx) => &src[idx..],
            None => "",
        }
    } else {
        src
    }
}

/// Parse and evaluate every form of `src` in `env`, driving each top-level
/// form on the shared LocalSet so async tasks make progress.
pub fn eval_str(env: &mut Env, src: &str, filename: &str) -> Result<Value, ExecError> {
    let mut parser = cljrs_reader::Parser::new(src.to_string(), filename.to_string());
    let forms = parser.parse_all().map_err(ExecError::Read)?;
    // Resolve top-level reader conditionals (nested ones are handled during
    // evaluation): `#?(:bb ...)` at the top of a script must run its branch.
    let forms = cljrs_builtins::form::expand_reader_conds(&forms);
    let mut result = Value::Nil;
    for form in forms {
        let _alloc_frame = cljrs_gc::push_alloc_frame();
        result = eval_form(&form, env).map_err(ExecError::Eval)?;
    }
    Ok(result)
}

/// Evaluate one form, driven on the shared LocalSet when available.
fn eval_form(form: &cljrs_reader::Form, env: &mut Env) -> Result<Value, EvalError> {
    crate::with_driver(|drv| match drv {
        Some((rt, local)) => local.block_on(rt, cljrs_async::eval_async::eval_async(form, env)),
        None => cljrs_interp::eval::eval(form, env),
    })
}

pub enum ExecError {
    Read(cljrs_types::error::CljxError),
    Eval(EvalError),
}

/// Print the error and return the process exit code.
///
/// - `EvalError::Exit(code)` (System/exit) exits silently with that code.
/// - A thrown ex-info whose data has `:cljrsh/exit` or `:babashka/exit`
///   exits with that code, printing only the message (babashka's clean-CLI
///   convention).
/// - Everything else prints an error report and exits 1.
fn report_error(e: ExecError) -> i32 {
    match e {
        ExecError::Read(err) => {
            eprintln!("cljrsh: read error: {err}");
            1
        }
        ExecError::Eval(EvalError::Exit(code)) => code,
        ExecError::Eval(EvalError::Thrown(val)) => {
            if let Some((code, message)) = requested_exit(&val) {
                if let Some(msg) = message
                    && !msg.is_empty()
                {
                    eprintln!("{msg}");
                }
                return code;
            }
            eprintln!("cljrsh: unhandled exception: {val}");
            1
        }
        ExecError::Eval(other) => {
            eprintln!("cljrsh: {other}");
            1
        }
    }
}

/// Extract `(:cljrsh/exit ex-data)` / `(:babashka/exit ex-data)` from a thrown
/// value, checking one `ex-cause` level deep (matching babashka).
fn requested_exit(val: &Value) -> Option<(i32, Option<String>)> {
    fn from_error(e: &cljrs_value::ExceptionInfo) -> Option<(i32, Option<String>)> {
        let data = e.data()?;
        for key in ["cljrsh/exit", "babashka/exit"] {
            if let Some(Value::Long(code)) = data.get(&Value::keyword(Keyword::parse(key))) {
                return Some((code as i32, Some(e.message())));
            }
        }
        None
    }
    match val {
        Value::Error(e) => from_error(e.get()).or_else(|| {
            e.get()
                .cause()
                .and_then(|cause| from_error(cause.get()))
        }),
        _ => None,
    }
}
