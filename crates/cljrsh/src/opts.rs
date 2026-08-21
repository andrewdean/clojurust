//! Command-line parsing for cljrsh: file-first, babashka-style.
//!
//! Grammar: `cljrsh [opts] (-e EXPR | -f FILE | FILE) [args...]`. Everything
//! after the chosen program (or after `--`) becomes `*command-line-args*`.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, PartialEq)]
pub enum Program {
    /// Evaluate an expression (`-e`); its non-nil result is printed.
    Eval(String),
    /// Run a script file (`-f FILE` or a bare existing path).
    File(String),
    /// Interactive REPL (explicit `--repl`, or no program on a tty).
    Repl,
    /// Read the program from stdin (no program given, stdin not a tty).
    Stdin,
    /// Print help / version and exit 0.
    Help,
    Version,
}

#[derive(Debug)]
pub struct Opts {
    pub program: Program,
    /// Values for `*command-line-args*`.
    pub args: Vec<String>,
    /// Extra directories for the source path (`-cp`/`--classpath`, `:`-separated).
    pub classpath: Vec<String>,
}

pub fn usage() -> String {
    format!(
        "cljrsh {VERSION} — Clojure Rust Shell

Usage: cljrsh [opts] [-e EXPR | -f FILE | FILE] [args...]

Options:
  -e, --eval EXPR       evaluate an expression (prints a non-nil result)
  -f, --file FILE       run a script file
  -cp, --classpath CP   additional source directories (colon-separated)
      --repl            start the interactive REPL
  -v, --version         print version
  -h, --help            this help

With no program: starts a REPL on a terminal, otherwise reads the script
from stdin. Remaining arguments are bound to *command-line-args*.

Environment: CLJRSH_PRELOADS (or BABASHKA_PRELOADS) is evaluated before
the program."
    )
}

pub fn parse(argv: &[String]) -> Result<Opts, String> {
    let mut program: Option<Program> = None;
    let mut args: Vec<String> = Vec::new();
    let mut classpath: Vec<String> = Vec::new();
    let mut i = 0;

    while i < argv.len() {
        let arg = &argv[i];
        match arg.as_str() {
            "-h" | "--help" => return Ok(finish(Some(Program::Help), args, classpath)),
            "-v" | "--version" => return Ok(finish(Some(Program::Version), args, classpath)),
            "--repl" if program.is_none() => {
                program = Some(Program::Repl);
                i += 1;
            }
            "-e" | "--eval" if program.is_none() => {
                let expr = argv
                    .get(i + 1)
                    .ok_or_else(|| format!("{arg} requires an expression"))?;
                program = Some(Program::Eval(expr.clone()));
                i += 2;
            }
            "-f" | "--file" if program.is_none() => {
                let file = argv
                    .get(i + 1)
                    .ok_or_else(|| format!("{arg} requires a file"))?;
                program = Some(Program::File(file.clone()));
                i += 2;
            }
            "-cp" | "--classpath" => {
                let cp = argv
                    .get(i + 1)
                    .ok_or_else(|| format!("{arg} requires a path list"))?;
                classpath.extend(cp.split(':').map(str::to_string));
                i += 2;
            }
            "--" => {
                args.extend(argv[i + 1..].iter().cloned());
                i = argv.len();
            }
            other => {
                if program.is_none() {
                    if other.starts_with('-') && other.len() > 1 {
                        return Err(format!("unknown option {other} (see cljrsh --help)"));
                    }
                    if !std::path::Path::new(other).exists() {
                        return Err(format!("file not found: {other}"));
                    }
                    program = Some(Program::File(other.to_string()));
                } else {
                    args.push(other.to_string());
                }
                i += 1;
            }
        }
    }

    Ok(finish(program, args, classpath))
}

fn finish(program: Option<Program>, args: Vec<String>, classpath: Vec<String>) -> Opts {
    use std::io::IsTerminal;
    let program = program.unwrap_or_else(|| {
        if std::io::stdin().is_terminal() {
            Program::Repl
        } else {
            Program::Stdin
        }
    });
    Opts {
        program,
        args,
        classpath,
    }
}
