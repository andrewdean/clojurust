//! Thread-local output-target stack.
//!
//! `with-out-str` captures and `*out*` stream redirection share one stack
//! so their dynamic extents compose: `binding` pushes a stream marker when
//! `*out*` is rebound to one of the IO sentinels (`:cljrs.io/stdout` /
//! `:cljrs.io/stderr`), and `with-out-str` pushes a capture buffer.
//!
//! Resolution order for a print (`emit`): the TOPMOST capture buffer wins,
//! even below a stream marker — a deliberate divergence from JVM Clojure,
//! where output redirected to stderr escapes `with-out-str`. Scripting
//! harnesses assert over a script's combined output, so `with-out-str`
//! capturing both streams is the more useful contract. With no capture
//! active, the topmost stream marker picks stdout or stderr (default
//! stdout).

use std::cell::RefCell;

pub enum OutputTarget {
    Capture(String),
    Stdout,
    Stderr,
}

thread_local! {
    static TARGETS: RefCell<Vec<OutputTarget>> = const { RefCell::new(Vec::new()) };
}

/// Push a capture buffer (`with-out-str` enter).
pub fn push_capture() {
    TARGETS.with(|t| t.borrow_mut().push(OutputTarget::Capture(String::new())));
}

/// Pop the top entry, returning its contents when it is a capture buffer
/// (`with-out-str` exit — stream markers pushed inside its body are popped
/// by their own binding guards before this runs).
pub fn pop_capture() -> Option<String> {
    TARGETS.with(|t| match t.borrow_mut().pop() {
        Some(OutputTarget::Capture(buf)) => Some(buf),
        _ => None,
    })
}

/// Push a stream marker (`binding` rebound `*out*` to an IO sentinel).
pub fn push_stream(stderr: bool) {
    TARGETS.with(|t| {
        t.borrow_mut().push(if stderr {
            OutputTarget::Stderr
        } else {
            OutputTarget::Stdout
        });
    });
}

/// Pop a stream marker (the binding guard's exit).
pub fn pop_stream() {
    TARGETS.with(|t| {
        t.borrow_mut().pop();
    });
}

fn resolve<'a>(stack: &'a mut [OutputTarget]) -> Result<&'a mut String, bool> {
    let mut stderr = None;
    for entry in stack.iter_mut().rev() {
        match entry {
            OutputTarget::Capture(buf) => return Ok(buf),
            OutputTarget::Stderr => stderr.get_or_insert(true),
            OutputTarget::Stdout => stderr.get_or_insert(false),
        };
    }
    Err(stderr.unwrap_or(false))
}

/// Write `s` to the current output target (no newline).
pub fn emit(s: &str) {
    TARGETS.with(|t| match resolve(&mut t.borrow_mut()) {
        Ok(buf) => buf.push_str(s),
        Err(true) => eprint!("{s}"),
        Err(false) => print!("{s}"),
    });
}

/// Write `s` plus a newline to the current output target.
pub fn emit_ln(s: &str) {
    TARGETS.with(|t| match resolve(&mut t.borrow_mut()) {
        Ok(buf) => {
            buf.push_str(s);
            buf.push('\n');
        }
        Err(true) => eprintln!("{s}"),
        Err(false) => println!("{s}"),
    });
}

/// Flush the stream the current target resolves to (no-op for captures).
pub fn flush_current() {
    use std::io::Write;
    TARGETS.with(|t| match resolve(&mut t.borrow_mut()) {
        Ok(_) => {}
        Err(true) => {
            let _ = std::io::stderr().flush();
        }
        Err(false) => {
            let _ = std::io::stdout().flush();
        }
    });
}
