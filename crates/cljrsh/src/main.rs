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

mod error;
mod exec;
mod opts;
mod repl;
mod tasks;
mod uberscript;

use opts::{Opts, Program};

/// 64 MiB, matching the `cljrs` binary's default interpreter stack.
const STACK_SIZE: usize = 256 * 1024 * 1024;

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
            // Clean pod shutdown before process::exit (which skips Drop).
            cljrsh_pods::shutdown_all();
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
        Program::Exec(fn_sym) => {
            // `-x my.ns/fn --flag val` — babashka's exec convention: the fn
            // receives one map of the remaining args parsed by babashka.cli.
            let require = match fn_sym.split_once('/') {
                Some((ns, _)) => format!("(require '{ns}) "),
                None => String::new(),
            };
            let src = format!(
                "{require}({fn_sym} (babashka.cli/parse-opts (vec (or *command-line-args* []))))"
            );
            let prelude = "(require 'babashka.cli) ";
            exec::run_program(
                &globals,
                &format!("{prelude}{src}"),
                "<exec>",
                modes(true),
            )
        }
        Program::Main(main_sym) => {
            // `-m my.ns [args…]` — require the namespace and apply its
            // `-main` (or `my.ns/fn`) to the remaining args as strings.
            let ns = main_sym.split('/').next().unwrap_or(&main_sym);
            let call = if main_sym.contains('/') {
                main_sym.clone()
            } else {
                format!("{main_sym}/-main")
            };
            let src =
                format!("(require '{ns}) (apply {call} (or *command-line-args* []))");
            exec::run_program(&globals, &src, "<main>", modes(false))
        }
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
            // here: task > subcommand (tasks shadow subcommands of the same
            // name, like babashka warns about).
            if let Some(project) = project
                .as_ref()
                .filter(|p| p.tasks.iter().any(|t| t.name == name))
            {
                return tasks::run(&globals, project, &name);
            }
            match name.as_str() {
                "nrepl-server" => nrepl(&globals, opts.args.first().map(String::as_str)),
                "socket-repl" => {
                    repl::socket(globals, opts.args.first().map(String::as_str))
                }
                "uberscript" => uberscript::run(&globals, &opts.args),
                "prepare" => prepare(&project),
                "describe" => describe(),
                "print-deps" => print_deps(&opts.args),
                "tasks" => match &project {
                    Some(project) => tasks::list(project),
                    None => {
                        eprintln!("cljrsh: no bb.edn project here");
                        2
                    }
                },
                "run" => {
                    // bb compat: accept --parallel but run sequentially (the
                    // interpreter is single-threaded by design).
                    let mut run_args = opts.args.clone();
                    if run_args.first().map(String::as_str) == Some("--parallel") {
                        eprintln!(
                            "cljrsh: warning: --parallel is not supported; running tasks sequentially"
                        );
                        run_args.remove(0);
                    }
                    match (&project, run_args.first()) {
                    (Some(project), Some(task)) => {
                        let task = task.clone();
                        // Remaining args become *command-line-args*.
                        cljrs_builtins::system::set_command_line_args(&globals, &run_args[1..]);
                        tasks::run(&globals, project, &task)
                    }
                    (None, _) => {
                        eprintln!("cljrsh: no bb.edn project here");
                        2
                    }
                    (_, None) => {
                        eprintln!("cljrsh: run requires a task name (see `cljrsh tasks`)");
                        2
                    }
                }},
                _ => {
                    if project.is_some() {
                        eprintln!(
                            "cljrsh: {name} is neither a file nor a task (see `cljrsh tasks`)"
                        );
                    } else {
                        eprintln!("cljrsh: file not found: {name}");
                    }
                    2
                }
            }
        }
        Program::Repl => repl::run(globals),
        Program::Help | Program::Version => unreachable!("handled before spawn"),
    }
}

/// `cljrsh prepare` — resolve and download the project's :deps ahead of
/// time (babashka's `bb prepare`). Pods download on first `load-pod`.
fn prepare(project: &Option<cljrsh_project::Project>) -> i32 {
    let Some(project) = project else {
        eprintln!("cljrsh: no bb.edn project here");
        return 2;
    };
    if project.deps.is_empty() {
        println!("Nothing to prepare (no :deps).");
        return 0;
    }
    match cljrsh_project::resolve_deps(project, &tasks::dep_cache_dir()) {
        Ok(dirs) => {
            println!("Prepared {} dependenc{}:", dirs.len(),
                     if dirs.len() == 1 { "y" } else { "ies" });
            for d in dirs {
                println!("  {}", d.display());
            }
            if !project.pods.is_empty() {
                println!("(pods download on first load-pod)");
            }
            0
        }
        Err(e) => {
            eprintln!("cljrsh: dependency resolution failed: {e}");
            1
        }
    }
}

/// `cljrsh print-deps [--format edn|classpath]` — the project's dependency
/// view, bb-compatible. `classpath` prints :paths plus every resolved dep
/// source directory joined with ':' (what clojure-lsp runs to index a
/// babashka-style project); `edn` (default, like bb) prints a deps map.
fn print_deps(args: &[String]) -> i32 {
    let format = match args {
        [] => "edn",
        [flag, value] if flag == "--format" => value.as_str(),
        _ => {
            eprintln!("usage: cljrsh print-deps [--format edn|classpath]");
            return 2;
        }
    };
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cljrsh: {e}");
            return 1;
        }
    };
    let project = cljrsh_project::find_project_file(&cwd)
        .and_then(|f| cljrsh_project::load(&f).ok());

    match format {
        "classpath" => {
            let mut entries: Vec<std::path::PathBuf> = Vec::new();
            if let Some(project) = &project {
                for p in &project.paths {
                    entries.push(project.root.join(p));
                }
                if entries.is_empty() {
                    // bb's implicit default source path.
                    let src = project.root.join("src");
                    if src.is_dir() {
                        entries.push(src);
                    }
                }
                if !project.deps.is_empty() {
                    match cljrsh_project::resolve_deps(project, &tasks::dep_cache_dir()) {
                        Ok(dirs) => entries.extend(dirs),
                        Err(e) => {
                            eprintln!("cljrsh: warning: dependency resolution failed: {e}");
                        }
                    }
                }
            }
            let rendered: Vec<String> = entries
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            println!("{}", rendered.join(":"));
            0
        }
        "edn" => {
            // {:paths [...] :deps {lib coord ...} :pods {lib {:version v}}}
            let paths = project
                .as_ref()
                .map(|p| p.paths.clone())
                .unwrap_or_default();
            print!("{{:paths [");
            print!(
                "{}",
                paths
                    .iter()
                    .map(|p| format!("{p:?}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            print!("] :deps {{");
            if let Some(project) = &project {
                let mut first = true;
                for (lib, coord) in &project.deps {
                    if !first {
                        print!(" ");
                    }
                    first = false;
                    match coord {
                        cljrsh_project::DepCoord::Local { root } => {
                            print!("{lib} {{:local/root {root:?}}}");
                        }
                        cljrsh_project::DepCoord::Git { url, sha } => {
                            print!("{lib} {{:git/url {url:?} :git/sha {sha:?}}}");
                        }
                        cljrsh_project::DepCoord::Maven { version } => {
                            print!("{lib} {{:mvn/version {version:?}}}");
                        }
                    }
                }
            }
            print!("}}");
            if let Some(project) = &project
                && !project.pods.is_empty()
            {
                print!(" :pods {{");
                let rendered: Vec<String> = project
                    .pods
                    .iter()
                    .map(|(lib, version)| format!("{lib} {{:version {version:?}}}"))
                    .collect();
                print!("{}", rendered.join(" "));
                print!("}}");
            }
            println!("}}");
            0
        }
        other => {
            eprintln!("cljrsh: unknown print-deps format {other:?} (edn|classpath)");
            2
        }
    }
}

/// `cljrsh describe` — machine-readable runtime info, bb-style EDN.
fn describe() -> i32 {
    println!(
        "{{:cljrsh/version {:?}
 :babashka/protocol-compat true
 :feature/reader-conds [:bb :cljrsh :clj :rust]
 :feature/nu {nu}
 :feature/aws {aws}
 :feature/k8s {k8s}
 :feature/pods true
 :feature/nrepl true
 :os/name {:?}
 :os/arch {:?}}}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        nu = cfg!(feature = "nu"),
        aws = cfg!(feature = "aws"),
        k8s = cfg!(feature = "k8s"),
    );
    0
}

/// `cljrsh nrepl-server [addr]` — addr as `PORT` or `HOST:PORT`; default
/// 127.0.0.1:1667 (babashka's port). Writes `.nrepl-port`; serves forever,
/// driving each form on the shared LocalSet so async code works in sessions.
fn nrepl(globals: &std::sync::Arc<cljrs_env::env::GlobalEnv>, addr: Option<&str>) -> i32 {
    let addr = match addr {
        None => "127.0.0.1:1667".to_string(),
        Some(a) if a.contains(':') => a.to_string(),
        Some(port) => format!("127.0.0.1:{port}"),
    };
    // Resolve hostnames too — editors ask for "localhost:0" (CIDER's
    // default babashka jack-in parameters). Prefer IPv4 (like babashka):
    // clients dial 127.0.0.1.
    use std::net::ToSocketAddrs as _;
    let addr: std::net::SocketAddr = match addr.to_socket_addrs().ok().and_then(|resolved| {
        let all: Vec<_> = resolved.collect();
        all.iter().find(|a| a.is_ipv4()).copied().or_else(|| all.first().copied())
    }) {
        Some(a) => a,
        None => {
            eprintln!("cljrsh: bad nREPL address {addr:?}");
            return 2;
        }
    };
    let config = cljrs_nrepl::Config {
        addr,
        port_file: Some(std::path::PathBuf::from(".nrepl-port")),
    };
    let server = match cljrs_nrepl::start(config, globals.clone()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cljrsh: {e}");
            return 1;
        }
    };
    println!(
        "nREPL server started on port {0} on host {1} - nrepl://{1}:{0}",
        server.port(),
        addr.ip()
    );
    match server.serve_with(exec::eval_form) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("cljrsh: nREPL server error: {e}");
            1
        }
    }
}
