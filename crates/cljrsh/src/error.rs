//! babashka-style error reports: Type / Message / Data / Location, a source
//! snippet with a caret, and the interpreted Clojure call trace.

use std::collections::HashMap;
use std::sync::Mutex;

use cljrs_env::error::EvalError;
use cljrs_interp::trace::Frame;
use cljrs_value::Value;

/// Sources the binary evaluated, keyed by the filename given to the reader
/// (`<expr>`, `<preloads>`, script paths, ...). Files not registered here
/// (e.g. `require`d from disk) are re-read on demand.
static SOURCES: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

pub fn register_source(filename: &str, src: &str) {
    let mut guard = SOURCES.lock().unwrap();
    guard
        .get_or_insert_with(HashMap::new)
        .insert(filename.to_string(), src.to_string());
}

fn lookup_source(filename: &str) -> Option<String> {
    if let Some(src) = SOURCES
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(filename).cloned())
    {
        return Some(src);
    }
    if !filename.starts_with('<') {
        return std::fs::read_to_string(filename).ok();
    }
    None
}

const RULE: &str = "----------------------------------------------------";

/// Render a full report for an escaping evaluation error to stderr.
pub fn report(e: &EvalError) {
    let trace = cljrs_interp::trace::take_error_trace().unwrap_or_default();
    let (type_name, message, data) = describe(e);

    eprintln!("----- Error {RULE}");
    eprintln!("Type:     {type_name}");
    eprintln!("Message:  {message}");
    if let Some(data) = data {
        eprintln!("Data:     {data}");
    }
    if let Some(frame) = trace.last() {
        eprintln!(
            "Location: {}:{}:{}",
            frame.span.file, frame.span.line, frame.span.col
        );
    }

    if let Some(frame) = trace.last()
        && let Some(context) = render_context(frame, &message)
    {
        eprintln!();
        eprintln!("----- Context {RULE}");
        eprint!("{context}");
    }

    if !trace.is_empty() {
        eprintln!();
        eprintln!("----- Stack trace {RULE}");
        // Innermost first, eliding the middle of very deep traces
        // (CLJRSH_FULL_TRACE=1 disables elision).
        let full = std::env::var("CLJRSH_FULL_TRACE").is_ok();
        let total = trace.len();
        for (shown, frame) in trace.iter().rev().enumerate() {
            if !full && total > 12 && shown == 6 {
                eprintln!("... {} frames elided ...", total - 12);
            }
            if !full && total > 12 && (6..total - 6).contains(&shown) {
                continue;
            }
            eprintln!("{}", format_frame(frame));
        }
    }
}

fn format_frame(frame: &Frame) -> String {
    format!(
        "{}/{} - {}:{}:{}",
        frame.ns, frame.name, frame.span.file, frame.span.line, frame.span.col
    )
}

/// ±2 lines around the failing line, with a caret under the column.
fn render_context(frame: &Frame, message: &str) -> Option<String> {
    let src = lookup_source(frame.span.file.as_str())?;
    snippet(
        &src,
        frame.span.line as usize,
        frame.span.col as usize,
        message,
    )
}

/// ±2 lines of `src` around `line_no`, with a caret under `col`.
fn snippet(src: &str, line_no: usize, col: usize, message: &str) -> Option<String> {
    let lines: Vec<&str> = src.lines().collect();
    if line_no == 0 || line_no > lines.len() {
        return None;
    }
    let first = line_no.saturating_sub(2).max(1);
    let last = (line_no + 2).min(lines.len());
    let width = last.to_string().len();
    let mut out = String::new();
    for n in first..=last {
        out.push_str(&format!("{n:width$}: {}\n", lines[n - 1]));
        if n == line_no {
            let pad = width + 2 + col.saturating_sub(1);
            out.push_str(&format!("{}^--- {message}\n", " ".repeat(pad)));
        }
    }
    Some(out)
}

/// Render a full report for a reader (parse) error to stderr, using the
/// span and source the reader attached to the error.
pub fn report_read(err: &cljrs_types::error::CljxError) {
    use cljrs_types::error::CljxError;
    let CljxError::ReadError { message, span, src } = err else {
        eprintln!("cljrsh: read error: {err}");
        return;
    };
    let source = src.inner().as_str();
    // Translate the byte offset into a 1-based line/column.
    let offset = span.map(|s| s.offset()).unwrap_or(0).min(source.len());
    let (mut line, mut col) = (1usize, 1usize);
    for ch in source[..offset].chars() {
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }

    eprintln!("----- Error {RULE}");
    eprintln!("Type:     Reader error");
    eprintln!("Message:  {message}");
    eprintln!("Location: {}:{line}:{col}", src.name());
    if let Some(context) = snippet(source, line, col, message) {
        eprintln!();
        eprintln!("----- Context {RULE}");
        eprint!("{context}");
    }
}

/// (type, message, data) for the banner.
fn describe(e: &EvalError) -> (String, String, Option<String>) {
    match e {
        EvalError::Thrown(val) => match val {
            Value::Error(err) => {
                let info = err.get();
                let data = info.data().map(|m| format!("{}", Value::Map(m)));
                let mut message = info.message();
                if let Some(cause) = info.cause() {
                    message.push_str(&format!(" (caused by: {})", cause.get().message()));
                }
                ("clojure.lang.ExceptionInfo".to_string(), message, data)
            }
            other => (
                format!("thrown {}", other.type_name()),
                format!("{other}"),
                None,
            ),
        },
        EvalError::UnboundSymbol(s) => (
            "Unresolved symbol".to_string(),
            format!("Unable to resolve symbol: {s} in this context"),
            None,
        ),
        EvalError::Arity {
            name,
            expected,
            got,
        } => (
            "Arity error".to_string(),
            format!("Wrong number of args ({got}) passed to {name}; expected {expected}"),
            None,
        ),
        EvalError::NotCallable(s) => (
            "Not callable".to_string(),
            format!("Cannot call {s} as a function"),
            None,
        ),
        EvalError::GasExhausted => (
            "Gas exhausted".to_string(),
            "the evaluation budget was exhausted".to_string(),
            None,
        ),
        other => ("Error".to_string(), other.to_string(), None),
    }
}
