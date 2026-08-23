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
        // Innermost first, eliding the middle of very deep traces.
        let total = trace.len();
        for (shown, frame) in trace.iter().rev().enumerate() {
            if total > 12 && shown == 6 {
                eprintln!("... {} frames elided ...", total - 12);
            }
            if total > 12 && (6..total - 6).contains(&shown) {
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
    let line_no = frame.span.line as usize;
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
            let pad = width + 2 + (frame.span.col as usize).saturating_sub(1);
            out.push_str(&format!("{}^--- {message}\n", " ".repeat(pad)));
        }
    }
    Some(out)
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
        EvalError::Arity { name, expected, got } => (
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
