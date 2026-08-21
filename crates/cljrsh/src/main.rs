// EvalError is the runtime's pervasive error type; the same allow is applied
// crate-wide in cljrs-builtins/interp for the same reason.
#![allow(clippy::result_large_err)]

//! cljrsh — Clojure Rust Shell: a babashka-style scripting binary on clojurust.
//!
//! File-first CLI (`cljrsh script.clj`, `cljrsh -e '…'`), shebang scripts,
//! preloads, babashka exit-code conventions, and a rustyline REPL. All
//! evaluation runs on one thread with a large stack (the tree-walker uses
//! real Rust stack) alongside a current-thread Tokio runtime + LocalSet that
//! drives async tasks per top-level form (the `crates/cljrs` idiom).

mod exec;
mod opts;
mod repl;
mod tasks;

use opts::{Opts, Program};

/// 64 MiB, matching the `cljrs` binary's default interpreter stack.
const STACK_SIZE: usize = 64 * 1024 * 1024;

struct AsyncDriver {
    rt: tokio::runtime::Runtime,
    local: tokio::task::LocalSet,
}

thread_local! {
    static ASYNC_DRIVER: std::cell::RefCell<Option<AsyncDriver>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `f` with the thread's async driver (`Some((&rt, &local))` once
/// installed). Modules use this to drive evaluation on the shared LocalSet.
pub(crate) fn with_driver<R>(
    f: impl FnOnce(Option<(&tokio::runtime::Runtime, &tokio::task::LocalSet)>) -> R,
) -> R {
    ASYNC_DRIVER.with(|d| {
        let guard = d.borrow();
        f(guard.as_ref().map(|drv| (&drv.rt, &drv.local)))
    })
}

fn main() {
    // Rust ignores SIGPIPE by default; a scripting tool must die quietly when
    // its stdout reader goes away (`cljrsh -e '(run! println (range))' | head`).
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let opts = match opts::parse(&argv) {
        Ok(o) => o,
        Err(msg) => {
            eprintln!("cljrsh: {msg}");
            std::process::exit(2);
        }
    };

    match &opts.program {
        Program::Help => {
            println!("{}", opts::usage());
            return;
        }
        Program::Version => {
            println!("cljrsh {}", opts::VERSION);
            return;
        }
        _ => {}
    }

    let handle = std::thread::Builder::new()
        .name("cljrsh-main".into())
        .stack_size(STACK_SIZE)
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build Tokio runtime");
            let local = tokio::task::LocalSet::new();
            ASYNC_DRIVER.with(|d| *d.borrow_mut() = Some(AsyncDriver { rt, local }));
            let code = run(opts);
            ASYNC_DRIVER.with(|d| *d.borrow_mut() = None);
            code
        })
        .expect("failed to spawn main thread");

    let code = handle.join().unwrap_or_else(|e| {
        eprintln!("cljrsh: thread panicked: {e:?}");
        101
    });
    std::process::exit(code);
}

fn run(opts: Opts) -> i32 {
    let _mutator = cljrs_gc::register_mutator();

    let extra_paths = opts
        .classpath
        .iter()
        .map(std::path::PathBuf::from)
        .collect();
    let globals = exec::setup_globals(extra_paths, &opts.args);
    // Nearest bb.edn/cljrsh.edn: contributes :paths for every program kind
    // and the task graph for FileOrTask.
    let project = tasks::load_project(&globals);

    let modes = |print_result: bool| exec::RunModes {
        input: opts.input,
        output: opts.output,
        stream: opts.stream,
        print_result,
    };

    match opts.program {
        Program::Eval(expr) => exec::run_program(&globals, &expr, "<expr>", modes(true)),
        Program::File(path) => {
            let src = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("cljrsh: cannot read {path}: {e}");
                    return 1;
                }
            };
            // Make requires relative to the script's directory work.
            if let Some(parent) = std::path::Path::new(&path).parent()
                && parent != std::path::Path::new("")
            {
                globals
                    .source_paths
                    .write()
                    .unwrap()
                    .push(parent.to_path_buf());
            }
            exec::run_program(&globals, &src, &path, modes(false))
        }
        Program::Stdin => {
            let mut src = String::new();
            use std::io::Read;
            if let Err(e) = std::io::stdin().read_to_string(&mut src) {
                eprintln!("cljrsh: cannot read stdin: {e}");
                return 1;
            }
            exec::run_program(&globals, &src, "<stdin>", modes(false))
        }
        Program::FileOrTask(name) => {
            // babashka order: an existing file was already matched in opts;
            // here: task > subcommand. `run <task>` and `tasks` are the
            // subcommands, shadowable by tasks of the same name.
            let Some(project) = project else {
                eprintln!("cljrsh: file not found: {name} (and no bb.edn project here)");
                return 2;
            };
            let has_task = |n: &str| project.tasks.iter().any(|t| t.name == n);
            if has_task(&name) {
                return tasks::run(&globals, &project, &name);
            }
            match name.as_str() {
                "tasks" => tasks::list(&project),
                "run" => match opts.args.first() {
                    Some(task) => {
                        let task = task.clone();
                        // Remaining args become *command-line-args*.
                        cljrs_builtins::system::set_command_line_args(&globals, &opts.args[1..]);
                        tasks::run(&globals, &project, &task)
                    }
                    None => {
                        eprintln!("cljrsh: run requires a task name (see `cljrsh tasks`)");
                        2
                    }
                },
                _ => {
                    eprintln!(
                        "cljrsh: {name} is neither a file nor a task (see `cljrsh tasks`)"
                    );
                    2
                }
            }
        }
        Program::Repl => repl::run(globals),
        Program::Help | Program::Version => unreachable!("handled before spawn"),
    }
}
