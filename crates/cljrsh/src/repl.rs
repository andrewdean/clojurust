//! Interactive REPL: rustyline line editing with naive multi-line support
//! (keeps reading while delimiters are unbalanced), history in
//! `~/.cache/cljrsh/history`.

use std::sync::Arc;

use cljrs_env::env::{Env, GlobalEnv};

use crate::exec::{ExecError, eval_str};

pub fn run(globals: Arc<GlobalEnv>) -> i32 {
    println!("cljrsh {} — :repl/quit or Ctrl-D to exit", crate::opts::VERSION);
    let mut rl = match rustyline::DefaultEditor::new() {
        Ok(rl) => rl,
        Err(e) => {
            eprintln!("cljrsh: cannot start REPL: {e}");
            return 1;
        }
    };
    let history_path = history_path();
    if let Some(p) = &history_path {
        let _ = rl.load_history(p);
    }

    let mut env = Env::new(globals, "user");
    let mut buffer = String::new();

    loop {
        let prompt = if buffer.is_empty() { "user=> " } else { "  ...  " };
        match rl.readline(prompt) {
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
                    Err(ExecError::Eval(e)) => eprintln!("error: {e}"),
                }
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

fn save_history(rl: &mut rustyline::DefaultEditor, path: &Option<std::path::PathBuf>) {
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
