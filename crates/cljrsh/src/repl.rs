//! Interactive REPL: rustyline line editing with naive multi-line support
//! (keeps reading while delimiters are unbalanced), history in
//! `~/.cache/cljrsh/history`.

use std::sync::{Arc, Mutex};

use cljrs_env::env::{Env, GlobalEnv};
use cljrs_value::{Keyword, Value};

use crate::exec::{ExecError, eval_str};

/// rustyline helper: tab completion over special forms, the current
/// namespace's interns/refers/aliases, loaded namespace names, and
/// `alias/`-qualified publics. Highlighting/hinting/validation are the
/// rustyline defaults.
struct ReplHelper {
    globals: Arc<GlobalEnv>,
    /// Mirrors `env.current_ns`; refreshed by the REPL loop after each eval
    /// (in-ns can change it mid-session).
    current_ns: Arc<Mutex<String>>,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || "*+!-_?<>=./&$%".contains(c)
}

fn word_start(line: &str, pos: usize) -> usize {
    let mut start = pos;
    for (i, c) in line[..pos].char_indices().rev() {
        if is_word_char(c) {
            start = i;
        } else {
            break;
        }
    }
    start
}

/// True when the var's metadata carries a truthy `:private`.
fn var_is_private(var: &cljrs_gc::GcPtr<cljrs_value::Var>) -> bool {
    let private_kw = Value::keyword(Keyword::parse("private"));
    match var.get().get_meta() {
        Some(Value::Map(m)) => m
            .get(&private_kw)
            .is_some_and(|v| !matches!(v, Value::Nil | Value::Bool(false))),
        _ => false,
    }
}

impl ReplHelper {
    fn completions(&self, word: &str) -> Vec<String> {
        let cur_ns = self.current_ns.lock().unwrap().clone();
        let mut out: Vec<String> = Vec::new();
        if let Some((prefix, rest)) = word.split_once('/') {
            // alias/… or full-ns/… — complete that namespace's publics.
            let target = self
                .globals
                .resolve_alias(&cur_ns, prefix)
                .unwrap_or_else(|| Arc::from(prefix));
            let namespaces = self.globals.namespaces.read().unwrap();
            if let Some(ns) = namespaces.get(&target) {
                for (name, var) in ns.get().interns.lock().unwrap().iter() {
                    if name.starts_with(rest) && !var_is_private(var) {
                        out.push(format!("{prefix}/{name}"));
                    }
                }
            }
        } else {
            for sf in cljrs_builtins::special::SPECIAL_FORMS {
                if sf.starts_with(word) {
                    out.push((*sf).to_string());
                }
            }
            let namespaces = self.globals.namespaces.read().unwrap();
            if let Some(ns) = namespaces.get(cur_ns.as_str()) {
                let ns = ns.get();
                for name in ns.interns.lock().unwrap().keys() {
                    if name.starts_with(word) {
                        out.push(name.to_string());
                    }
                }
                for name in ns.refers.lock().unwrap().keys() {
                    if name.starts_with(word) {
                        out.push(name.to_string());
                    }
                }
                for alias in ns.aliases.lock().unwrap().keys() {
                    if alias.starts_with(word) {
                        out.push(format!("{alias}/"));
                    }
                }
            }
            // Namespace names (loaded and lazily-registered builtins) — for
            // `(require '…` and fully-qualified symbols.
            for name in namespaces.keys() {
                if name.starts_with(word) {
                    out.push(name.to_string());
                }
            }
            for name in self.globals.builtin_sources.read().unwrap().keys() {
                if name.starts_with(word) {
                    out.push(name.to_string());
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

impl rustyline::completion::Completer for ReplHelper {
    type Candidate = String;
    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<String>)> {
        let start = word_start(line, pos);
        let word = &line[start..pos];
        // Keywords (and the empty word) get no candidates.
        if word.is_empty() || word.starts_with(':') {
            return Ok((start, Vec::new()));
        }
        Ok((start, self.completions(word)))
    }
}

impl rustyline::hint::Hinter for ReplHelper {
    type Hint = String;
}
impl rustyline::highlight::Highlighter for ReplHelper {}
impl rustyline::validate::Validator for ReplHelper {}
impl rustyline::Helper for ReplHelper {}

type ReplEditor = rustyline::Editor<ReplHelper, rustyline::history::DefaultHistory>;

pub fn run(globals: Arc<GlobalEnv>) -> i32 {
    println!("cljrsh {} — :repl/quit or Ctrl-D to exit", crate::opts::VERSION);
    let mut rl: ReplEditor = match rustyline::Editor::new() {
        Ok(rl) => rl,
        Err(e) => {
            eprintln!("cljrsh: cannot start REPL: {e}");
            return 1;
        }
    };
    let current_ns = Arc::new(Mutex::new("user".to_string()));
    rl.set_helper(Some(ReplHelper {
        globals: globals.clone(),
        current_ns: current_ns.clone(),
    }));
    let history_path = history_path();
    if let Some(p) = &history_path {
        let _ = rl.load_history(p);
    }

    let mut env = Env::new(globals, "user");
    let mut buffer = String::new();

    loop {
        let prompt = if buffer.is_empty() {
            format!("{}=> ", env.current_ns)
        } else {
            "  ...  ".to_string()
        };
        match rl.readline(&prompt) {
            Ok(line) => {
                if buffer.is_empty() && line.trim() == ":repl/quit" {
                    break;
                }
                buffer.push_str(&line);
                buffer.push('\n');
                if delimiters_open(&buffer) {
                    continue;
                }
                let input = std::mem::take(&mut buffer);
                if input.trim().is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(input.trim_end());
                match eval_str(&mut env, &input, "<repl>") {
                    Ok(v) => println!("{v}"),
                    Err(ExecError::Read(e)) => eprintln!("read error: {e}"),
                    Err(ExecError::Eval(cljrs_env::error::EvalError::Exit(code))) => {
                        save_history(&mut rl, &history_path);
                        return code;
                    }
                    Err(ExecError::Eval(cljrs_env::error::EvalError::Interrupted)) => {
                        eprintln!("Interrupted.");
                    }
                    Err(ExecError::Eval(e)) => eprintln!("error: {e}"),
                }
                crate::clear_interrupt();
                *current_ns.lock().unwrap() = env.current_ns.to_string();
            }
            Err(rustyline::error::ReadlineError::Interrupted) => {
                buffer.clear();
                continue;
            }
            Err(rustyline::error::ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("cljrsh: readline error: {e}");
                break;
            }
        }
    }
    save_history(&mut rl, &history_path);
    0
}

fn history_path() -> Option<std::path::PathBuf> {
    let dir = std::env::var("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .ok()?
        .join("cljrsh");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("history"))
}

fn save_history(rl: &mut ReplEditor, path: &Option<std::path::PathBuf>) {
    if let Some(p) = path {
        let _ = rl.save_history(p);
    }
}

/// True while `(`/`[`/`{` remain unclosed (outside strings/comments), i.e.
/// the reader would hit EOF — keep accepting lines. A negative balance is
/// left for the reader to report as an error.
fn delimiters_open(src: &str) -> bool {
    let mut depth: i64 = 0;
    let mut in_string = false;
    let mut in_comment = false;
    let mut escaped = false;
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        if in_comment {
            if c == '\n' {
                in_comment = false;
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            ';' => in_comment = true,
            '\\' => {
                // Character literal: consume the next char so `\(` doesn't count.
                chars.next();
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    depth > 0 || in_string
}

#[cfg(test)]
mod tests {
    use super::delimiters_open;

    #[test]
    fn balance_tracking() {
        assert!(delimiters_open("(defn f [x]"));
        assert!(!delimiters_open("(defn f [x] x)"));
        assert!(delimiters_open("\"unterminated"));
        assert!(!delimiters_open("\"a ( b\""));
        assert!(!delimiters_open("; comment (\n1"));
        assert!(!delimiters_open("\\( 1"));
    }
}

// ── Socket REPL ───────────────────────────────────────────────────────────────

/// `cljrsh socket-repl [addr]` — a plain text REPL over TCP (babashka's
/// --socket-repl; default port 1666). One connection at a time (the
/// interpreter is single-threaded); each connection gets a fresh `user`
/// environment sharing the process globals.
pub fn socket(globals: Arc<GlobalEnv>, addr: Option<&str>) -> i32 {
    let addr = match addr {
        None => "127.0.0.1:1666".to_string(),
        Some(a) if a.contains(':') => a.to_string(),
        Some(port) => format!("127.0.0.1:{port}"),
    };
    let listener = match std::net::TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cljrsh: cannot bind socket REPL on {addr}: {e}");
            return 1;
        }
    };
    println!("Socket REPL started at {addr}");
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        if let Err(e) = serve_connection(&globals, stream) {
            eprintln!("cljrsh: socket REPL connection error: {e}");
        }
    }
    0
}

fn serve_connection(
    globals: &Arc<GlobalEnv>,
    stream: std::net::TcpStream,
) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Write};
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut env = Env::new(globals.clone(), "user");
    let mut buffer = String::new();
    write!(writer, "user=> ")?;
    writer.flush()?;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(()); // client hung up
        }
        buffer.push_str(&line);
        if buffer.trim().is_empty() {
            buffer.clear();
            write!(writer, "user=> ")?;
            writer.flush()?;
            continue;
        }
        let mut parser =
            cljrs_reader::Parser::new(buffer.clone(), "<socket-repl>".into());
        match parser.parse_all() {
            Ok(forms) => {
                buffer.clear();
                for form in &forms {
                    let _alloc_frame = cljrs_gc::push_alloc_frame();
                    // Evaluation prints (println etc.) go to the process's
                    // stdout — only results and errors travel the socket,
                    // like babashka's socket REPL.
                    match crate::exec::eval_form(form, &mut env) {
                        Ok(v) => writeln!(writer, "{v}")?,
                        Err(cljrs_env::error::EvalError::Exit(_)) => return Ok(()),
                        Err(cljrs_env::error::EvalError::Interrupted) => {
                            crate::clear_interrupt();
                            writeln!(writer, "Interrupted.")?;
                        }
                        Err(e) => writeln!(writer, "ERROR: {e}")?,
                    }
                }
                write!(writer, "user=> ")?;
                writer.flush()?;
            }
            Err(e) => {
                let msg = e.to_string();
                // An unclosed form means the expression continues on the
                // next line; anything else is a real syntax error.
                if !(msg.contains("unclosed") || msg.contains("unterminated")) {
                    buffer.clear();
                    writeln!(writer, "ERROR: {msg}")?;
                    write!(writer, "user=> ")?;
                    writer.flush()?;
                }
            }
        }
    }
}
