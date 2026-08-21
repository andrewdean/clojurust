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
    /// A bare argument that is not an existing file: a project task name, or
    /// the `tasks`/`run` subcommand. Resolved by `main` once the project file
    /// is loaded (babashka's file > task > subcommand order).
    FileOrTask(String),
    /// Interactive REPL (explicit `--repl`, or no program on a tty).
    Repl,
    /// Read the program from stdin (no program given, stdin not a tty).
    Stdin,
    /// Print help / version and exit 0.
    Help,
    Version,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputMode {
    /// `-i`: `*input*` is a lazy seq of stdin lines.
    Lines,
    /// `-I`: `*input*` is the seq of EDN values read from stdin.
    Edn,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputMode {
    /// `-o`: print each element of the result with println.
    Println,
    /// `-O`: print each element of the result with prn.
    Prn,
}

#[derive(Debug)]
pub struct Opts {
    pub program: Program,
    /// Values for `*command-line-args*`.
    pub args: Vec<String>,
    /// Extra directories for the source path (`-cp`/`--classpath`, `:`-separated).
    pub classpath: Vec<String>,
    /// `-i`/`-I` (and the combined `-io`-style flags).
    pub input: Option<InputMode>,
    /// `-o`/`-O`.
    pub output: Option<OutputMode>,
    /// `--stream`: re-evaluate the program per stdin line (or EDN value with `-I`).
    pub stream: bool,
}

pub fn usage() -> String {
    format!(
        "cljrsh {VERSION} — Clojure Rust Shell

Usage: cljrsh [opts] [-e EXPR | -f FILE | FILE] [args...]

Options:
  -e, --eval EXPR       evaluate an expression (prints a non-nil result)
  -f, --file FILE       run a script file
  -i                    bind *input* to a lazy seq of stdin lines
  -I                    bind *input* to the seq of EDN values from stdin
  -o                    print each element of the result (println)
  -O                    print each element of the result (prn)
  -io -iO -Io -IO       combined input/output flags
      --stream          re-evaluate per stdin line (or EDN value with -I)
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

#[derive(Default)]
struct Modes {
    input: Option<InputMode>,
    output: Option<OutputMode>,
    stream: bool,
}

pub fn parse(argv: &[String]) -> Result<Opts, String> {
    let mut program: Option<Program> = None;
    let mut args: Vec<String> = Vec::new();
    let mut classpath: Vec<String> = Vec::new();
    let mut modes = Modes::default();
    let mut i = 0;

    while i < argv.len() {
        let arg = &argv[i];
        match arg.as_str() {
            "-h" | "--help" => return Ok(finish(Some(Program::Help), args, classpath, modes)),
            "-v" | "--version" => {
                return Ok(finish(Some(Program::Version), args, classpath, modes));
            }
            "-i" | "-I" | "-o" | "-O" | "-io" | "-iO" | "-Io" | "-IO" => {
                for c in arg.chars().skip(1) {
                    match c {
                        'i' => modes.input = Some(InputMode::Lines),
                        'I' => modes.input = Some(InputMode::Edn),
                        'o' => modes.output = Some(OutputMode::Println),
                        'O' => modes.output = Some(OutputMode::Prn),
                        _ => unreachable!(),
                    }
                }
                i += 1;
            }
            "--stream" => {
                modes.stream = true;
                i += 1;
            }
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
                    program = Some(if std::path::Path::new(other).exists() {
                        Program::File(other.to_string())
                    } else {
                        Program::FileOrTask(other.to_string())
                    });
                } else {
                    args.push(other.to_string());
                }
                i += 1;
            }
        }
    }

    Ok(finish(program, args, classpath, modes))
}

fn finish(
    program: Option<Program>,
    args: Vec<String>,
    classpath: Vec<String>,
    modes: Modes,
) -> Opts {
    use std::io::IsTerminal;
    let program = program.unwrap_or_else(|| {
        // With streaming flags present stdin is data, not the program, so
        // fall back to the REPL as if on a terminal.
        if modes.input.is_some() || modes.stream || std::io::stdin().is_terminal() {
            Program::Repl
        } else {
            Program::Stdin
        }
    });
    Opts {
        program,
        args,
        classpath,
        input: modes.input,
        output: modes.output,
        stream: modes.stream,
    }
}
