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
    cljrs_builtins::form::set_reader_features(["bb", "cljrsh", "clj", "rust"]);

    // gc_config_from_env, not GcConfig::new: this call overrides the config
    // standard_env() derived from the environment, so passing the bare
    // defaults here silently made CLJRS_GC_SOFT_LIMIT_MB /
    // CLJRS_GC_HARD_LIMIT_MB dead for cljrsh (they worked for cljrs).
    let globals = cljrs_stdlib::standard_env_with_paths_and_config(
        extra_paths,
        Arc::new(cljrs_gc::gc_config_from_env()),
    );

    // Async runtime hookups run inside the LocalSet (init spawns tasks).
    crate::with_driver(|drv| {
        let init = |g: &Arc<GlobalEnv>| {
            cljrs_async::init(g);
            cljrs_io::init(g);
            cljrs_net::init(g);
            cljrs_charset::init(g);
        };
        match drv {
            Some((rt, local)) => local.block_on(rt, async { init(&globals) }),
            None => init(&globals),
        }
    });
    cljrs_process::init(&globals);
    cljrsh_host::init(&globals);
    cljrsh_pods::init(&globals);
    #[cfg(feature = "nu")]
    cljrsh_nu::init(&globals);
    #[cfg(feature = "aws")]
    cljrsh_aws::init(&globals);
    cljrs_base64::init(&globals);
    #[cfg(feature = "k8s")]
    {
        cljrsh_k8s::init(&globals);
        cljrs_async::load_source(&globals, "k8s", cljrsh_k8s::K8S_SUGAR);
    }

    cljrs_builtins::system::set_command_line_args(&globals, args);
    globals
}

/// How a program's stdin/result should be wired (`-i/-I/-o/-O/--stream`).
pub struct RunModes {
    pub input: Option<crate::opts::InputMode>,
    pub output: Option<crate::opts::OutputMode>,
    pub stream: bool,
    /// Print the final non-nil value (the `-e` behavior) when no output mode.
    pub print_result: bool,
}

/// Evaluate `src` as a program: strip a shebang line, run preloads first,
/// bind `*input*` per the input mode, evaluate (once, or per stdin value with
/// `--stream`), print per the output mode, and map the outcome to an exit code.
pub fn run_program(globals: &Arc<GlobalEnv>, src: &str, filename: &str, modes: RunModes) -> i32 {
    let mut env = Env::new(globals.clone(), "user");

    if let Some(preloads) = preloads() {
        match eval_str(&mut env, &preloads, "<preloads>") {
            Ok(_) => {}
            Err(e) => return report_error(e),
        }
    }

    let src = strip_shebang(src);

    if modes.stream {
        return run_stream(globals, &mut env, src, filename, &modes);
    }

    if let Some(mode) = modes.input {
        let def = match mode {
            crate::opts::InputMode::Lines => {
                "(def *input* ((fn line-seq* []
                    (lazy-seq (when-let [l (cljrsh.io/stdin-read-line)]
                                (cons l (line-seq*)))))))"
            }
            crate::opts::InputMode::Edn => {
                "(def *input* (seq (cljrsh.io/read-edn-all (cljrsh.io/stdin-read-all))))"
            }
        };
        if let Err(e) = eval_str(&mut env, def, "<input>") {
            return report_error(e);
        }
    }

    match eval_str(&mut env, src, filename) {
        Ok(value) => {
            if let Some(out) = modes.output {
                if let Err(e) = print_output(globals, &mut env, value, out) {
                    return report_error(e);
                }
            } else if modes.print_result && value != Value::Nil {
                println!("{value}");
            }
            0
        }
        Err(e) => report_error(e),
    }
}

/// `--stream`: read one line per iteration (parsed as a single EDN value with
/// `-I`), bind it as `*input*`, evaluate the program, and print the result
/// (element-wise with `-o`/`-O`, otherwise prn of non-nil).
fn run_stream(
    globals: &Arc<GlobalEnv>,
    env: &mut Env,
    src: &str,
    filename: &str,
    modes: &RunModes,
) -> i32 {
    let edn = matches!(modes.input, Some(crate::opts::InputMode::Edn));
    while let Some(line) = cljrsh_host::io::read_line() {
        let value = if edn {
            match cljrsh_host::io::read_edn_one(&line, "<stdin>") {
                Ok(Some(v)) => v,
                Ok(None) => continue,
                Err(e) => {
                    eprintln!("cljrsh: {e}");
                    return 1;
                }
            }
        } else {
            Value::string(line)
        };
        globals.intern("user", Arc::from("*input*"), value);
        match eval_str(env, src, filename) {
            Ok(result) => {
                if let Some(out) = modes.output {
                    if let Err(e) = print_output(globals, env, result, out) {
                        return report_error(e);
                    }
                } else if result != Value::Nil {
                    let _ = eval_with_value(globals, env, result, "(prn cljrsh-out*)");
                }
            }
            Err(e) => return report_error(e),
        }
    }
    0
}

/// Print `value` element-wise: collections one element per line, scalars as a
/// single line; `-o` uses println (bare strings), `-O` uses prn (readable).
fn print_output(
    globals: &Arc<GlobalEnv>,
    env: &mut Env,
    value: Value,
    mode: crate::opts::OutputMode,
) -> Result<(), ExecError> {
    let printer = match mode {
        crate::opts::OutputMode::Println => {
            "(run! println (if (or (nil? cljrsh-out*) (coll? cljrsh-out*) (seq? cljrsh-out*)) cljrsh-out* [cljrsh-out*]))"
        }
        crate::opts::OutputMode::Prn => {
            "(run! prn (if (or (nil? cljrsh-out*) (coll? cljrsh-out*) (seq? cljrsh-out*)) cljrsh-out* [cljrsh-out*]))"
        }
    };
    eval_with_value(globals, env, value, printer).map(|_| ())
}

/// Evaluate `src` with `value` bound to the var `cljrsh-out*` in `user`.
fn eval_with_value(
    globals: &Arc<GlobalEnv>,
    env: &mut Env,
    value: Value,
    src: &str,
) -> Result<Value, ExecError> {
    globals.intern("user", Arc::from("cljrsh-out*"), value);
    eval_str(env, src, "<output>")
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
    crate::error::register_source(filename, src);
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
/// CLJRSH_GAS=<credits> caps evaluation steps per top-level form — runaway
/// recursion then reports GasExhausted with a Clojure stack trace instead of
/// dying in a native stack overflow.
pub fn eval_form(form: &cljrs_reader::Form, env: &mut Env) -> Result<Value, EvalError> {
    let gas: Option<u64> = std::env::var("CLJRSH_GAS")
        .ok()
        .and_then(|v| v.parse().ok());
    crate::with_driver(|drv| match (drv, gas) {
        (Some((rt, local)), _) => {
            let _guard =
                gas.map(|g| cljrs_env::gas::GasGuard::install(cljrs_env::gas::GasMeter::new(g)));
            local.block_on(rt, cljrs_async::eval_async::eval_async(form, env))
        }
        (None, Some(g)) => cljrs_interp::eval::eval_with_gas(form, env, g),
        (None, None) => cljrs_interp::eval::eval(form, env),
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
pub fn report_error(e: ExecError) -> i32 {
    match e {
        ExecError::Read(err) => {
            crate::error::report_read(&err);
            1
        }
        ExecError::Eval(EvalError::Exit(code)) => code,
        // SIGINT: conventional 128+SIGINT exit; finally blocks already ran.
        ExecError::Eval(EvalError::Interrupted) => {
            eprintln!("Interrupted.");
            130
        }
        ExecError::Eval(EvalError::Thrown(val)) => {
            if let Some((code, message)) = requested_exit(&val) {
                if let Some(msg) = message
                    && !msg.is_empty()
                {
                    eprintln!("{msg}");
                }
                return code;
            }
            crate::error::report(&EvalError::Thrown(val));
            1
        }
        ExecError::Eval(other) => {
            crate::error::report(&other);
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
        Value::Error(e) => from_error(e.get())
            .or_else(|| e.get().cause().and_then(|cause| from_error(cause.get()))),
        _ => None,
    }
}
