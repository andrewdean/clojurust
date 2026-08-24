//! bb.edn task execution: helpers interned into the task namespace,
//! `:init`, `:depends`-ordered evaluation with each task's result bound
//! under its name (babashka semantics, interpreted structurally).

use std::sync::Arc;

use cljrs_env::env::{Env, GlobalEnv};
use cljrs_reader::{Form, FormKind};
use cljrsh_project::{Project, ProjectError};

use crate::exec::{ExecError, eval_str};

/// Helpers auto-interned before task bodies run (unless the project shadows
/// them): babashka.tasks' `shell` / `sh`; `clojure` errors by design (no JVM).
const TASK_PRELUDE: &str = "
(require '[babashka.process])
(def shell babashka.process/shell)
(def sh babashka.process/sh)
(defn clojure [& _]
  (throw (ex-info \"babashka.tasks/clojure requires a JVM; cljrsh has none\" {})))
(def ^:dynamic *current-task* nil)
(defn current-task [] *current-task*)
";

/// List tasks bb-style. Returns the process exit code.
pub fn list(project: &Project) -> i32 {
    let public: Vec<_> = project.tasks.iter().filter(|t| !t.private).collect();
    if public.is_empty() {
        println!("No tasks found.");
        return 0;
    }
    println!("The following tasks are available:");
    println!();
    let width = public.iter().map(|t| t.name.len()).max().unwrap_or(0);
    for task in public {
        match &task.doc {
            Some(doc) => println!("{:width$} {}", task.name, doc),
            None => println!("{}", task.name),
        }
    }
    0
}

/// Run `target` and its `:depends` closure in topological order. Each task's
/// result is interned under its name in `user`, so downstream bodies can
/// refer to upstream results. Returns the process exit code.
pub fn run(globals: &Arc<GlobalEnv>, project: &Project, target: &str) -> i32 {
    let order = match cljrsh_project::task_order(project, target) {
        Ok(order) => order,
        Err(e) => {
            eprintln!("cljrsh: {e}");
            return 1;
        }
    };

    let mut env = Env::new(globals.clone(), "user");
    if let Err(e) = eval_str(&mut env, TASK_PRELUDE, "<task-prelude>") {
        return crate::exec::report_error(e);
    }
    if let Some(init) = &project.init
        && let Err(e) = eval_form_checked(init, &mut env)
    {
        return crate::exec::report_error(e);
    }

    if let Err(e) = eval_requires(&project.requires, &mut env) {
        return crate::exec::report_error(e);
    }

    for task in order {
        if let Err(e) = eval_requires(&task.requires, &mut env) {
            return crate::exec::report_error(e);
        }
        // (current-task) → this task's map, for :enter/:leave/body.
        globals.intern("user", Arc::from("*current-task*"), task_map(task));
        // Task-local :enter/:leave override the :tasks-level hooks.
        if let Some(enter) = task.enter.as_ref().or(project.enter.as_ref())
            && let Err(e) = eval_form_checked(enter, &mut env)
        {
            return crate::exec::report_error(e);
        }
        // A bare symbol body names a fn to call: {:task my.ns/main}.
        let body = match &task.body.kind {
            FormKind::Symbol(_) => Form::new(
                FormKind::List(vec![task.body.clone()]),
                task.body.span.clone(),
            ),
            _ => task.body.clone(),
        };
        match eval_form_checked(&body, &mut env) {
            Ok(result) => {
                globals.intern("user", Arc::from(task.name.as_str()), result);
            }
            Err(e) => return crate::exec::report_error(e),
        }
        if let Some(leave) = task.leave.as_ref().or(project.leave.as_ref())
            && let Err(e) = eval_form_checked(leave, &mut env)
        {
            return crate::exec::report_error(e);
        }
    }
    0
}

/// The map (current-task) returns: {:name <symbol> [:doc <str>] [:private true]}.
fn task_map(task: &cljrsh_project::TaskDef) -> cljrs_value::Value {
    use cljrs_value::value::MapValue;
    use cljrs_value::{Keyword, Symbol, Value};
    let mut pairs = vec![(
        Value::keyword(Keyword::simple("name")),
        Value::symbol(Symbol::parse(&task.name)),
    )];
    if let Some(doc) = &task.doc {
        pairs.push((
            Value::keyword(Keyword::simple("doc")),
            Value::string(doc.clone()),
        ));
    }
    if task.private {
        pairs.push((
            Value::keyword(Keyword::simple("private")),
            Value::Bool(true),
        ));
    }
    Value::Map(MapValue::from_pairs(pairs))
}

/// Evaluate `(require '<spec>)` for each `:requires` libspec form.
fn eval_requires(specs: &[Form], env: &mut Env) -> Result<(), ExecError> {
    for spec in specs {
        let span = spec.span.clone();
        let quoted = Form::new(
            FormKind::List(vec![
                Form::new(FormKind::Symbol("quote".to_string()), span.clone()),
                spec.clone(),
            ]),
            span.clone(),
        );
        let call = Form::new(
            FormKind::List(vec![
                Form::new(FormKind::Symbol("require".to_string()), span.clone()),
                quoted,
            ]),
            span,
        );
        eval_form_checked(&call, env)?;
    }
    Ok(())
}

fn eval_form_checked(form: &Form, env: &mut Env) -> Result<cljrs_value::Value, ExecError> {
    let _alloc_frame = cljrs_gc::push_alloc_frame();
    crate::exec::eval_form(form, env).map_err(ExecError::Eval)
}

/// `~/.cache/cljrsh` (or `$XDG_CACHE_HOME/cljrsh`).
pub(crate) fn dep_cache_dir() -> std::path::PathBuf {
    std::env::var("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|_| std::path::PathBuf::from(".cljrsh-cache"))
        .join("cljrsh")
}

/// Load the nearest project file, apply its `:paths` to the source path, and
/// warn about `:min-bb-version` (informational — we are not babashka).
pub fn load_project(globals: &Arc<GlobalEnv>) -> Option<Project> {
    let cwd = std::env::current_dir().ok()?;
    let file = cljrsh_project::find_project_file(&cwd)?;
    match cljrsh_project::load(&file) {
        Ok(project) => {
            {
                let mut paths = globals.source_paths.write().unwrap();
                for p in &project.paths {
                    let resolved = project.root.join(p);
                    if !paths.contains(&resolved) {
                        paths.push(resolved);
                    }
                }
            }
            // :deps — local/git/maven, resolved into the source path. A
            // failure warns rather than aborting: tasks that don't touch the
            // dep still work, and requires of it fail with a clear message.
            if !project.deps.is_empty() {
                match cljrsh_project::resolve_deps(&project, &dep_cache_dir()) {
                    Ok(dirs) => {
                        let mut paths = globals.source_paths.write().unwrap();
                        for dir in dirs {
                            if !paths.contains(&dir) {
                                paths.push(dir);
                            }
                        }
                    }
                    Err(e) => eprintln!("cljrsh: warning: dependency resolution failed: {e}"),
                }
            }
            // :pods — registry pods, resolved (downloading on first use) and
            // loaded before any evaluation so their namespaces are requirable.
            for (name, version) in &project.pods {
                let loaded =
                    cljrsh_project::pods::ensure_registry_pod(name, version, &dep_cache_dir())
                        .and_then(|exe| {
                            cljrsh_pods::load_registry_pod(globals, &exe.to_string_lossy())
                        });
                if let Err(e) = loaded {
                    eprintln!("cljrsh: warning: pod {name} {version}: {e}");
                }
            }
            Some(project)
        }
        Err(e @ ProjectError::Parse(_)) | Err(e @ ProjectError::Shape(_)) => {
            eprintln!("cljrsh: warning: {e} ({})", file.display());
            None
        }
        Err(e) => {
            eprintln!("cljrsh: warning: {e}");
            None
        }
    }
}
