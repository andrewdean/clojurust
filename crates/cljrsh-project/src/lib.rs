//! bb.edn project-file support for cljrsh.
//!
//! Parses `bb.edn` (babashka's project file, read as-is) into plain data the
//! binary acts on: `:paths` for the source path, and `:tasks` as a graph of
//! [`TaskDef`]s whose bodies stay unevaluated [`Form`]s — the binary
//! evaluates them in `:depends` topological order, interning each task's
//! result under its name so later tasks can refer to earlier results
//! (babashka's semantics, interpreted structurally instead of via source
//! splicing).
//!
//! An optional `cljrsh.edn` in the same directory wins over `bb.edn`.

use std::path::{Path, PathBuf};

use cljrs_reader::{Form, FormKind};

pub mod maven;
pub mod pods;

/// A parsed project file.
#[derive(Debug)]
pub struct Project {
    /// Directory containing the project file (relative :paths resolve here).
    pub root: PathBuf,
    /// The project file itself.
    pub file: PathBuf,
    /// `:paths` — source directories, project-root-relative or absolute.
    pub paths: Vec<String>,
    /// `:min-bb-version` — warn-only (we are not babashka).
    pub min_bb_version: Option<String>,
    /// `:deps` — lib symbol → coordinate, in source order.
    pub deps: Vec<(String, DepCoord)>,
    /// `:tasks`, in source order.
    pub tasks: Vec<TaskDef>,
    /// `:tasks`' `:init` form, evaluated once before any task body.
    pub init: Option<Form>,
    /// `:pods` — registry pods to load before evaluation: (name, version).
    pub pods: Vec<(String, String)>,
}

/// A `:deps` coordinate.
#[derive(Debug, Clone, PartialEq)]
pub enum DepCoord {
    /// `{:local/root "path"}` — path relative to the project root.
    Local { root: String },
    /// `{:git/url "..." :git/sha "..."}`.
    Git { url: String, sha: String },
    /// `{:mvn/version "1.2.3"}` — group/artifact from the lib symbol
    /// (`group/artifact`, or `name` meaning `name/name`).
    Maven { version: String },
}

/// One entry under `:tasks`.
#[derive(Debug)]
pub struct TaskDef {
    pub name: String,
    pub doc: Option<String>,
    pub depends: Vec<String>,
    /// The body to evaluate: the task's expression form (or the `:task`
    /// value of a map entry). A bare symbol body resolves to a fn and calls it.
    pub body: Form,
    /// `:private` tasks are hidden from `cljrsh tasks`.
    pub private: bool,
}

#[derive(Debug)]
pub enum ProjectError {
    Io(String),
    Parse(String),
    Shape(String),
    /// Cycle in `:depends`, with the offending task name.
    Cycle(String),
    UnknownTask(String),
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectError::Io(e) => write!(f, "cannot read project file: {e}"),
            ProjectError::Parse(e) => write!(f, "cannot parse project file: {e}"),
            ProjectError::Shape(e) => write!(f, "malformed project file: {e}"),
            ProjectError::Cycle(t) => write!(f, "cyclic :depends involving task {t}"),
            ProjectError::UnknownTask(t) => write!(f, "no such task: {t} (see `cljrsh tasks`)"),
        }
    }
}

impl std::error::Error for ProjectError {}

/// Find the nearest project file walking up from `start`: `cljrsh.edn` wins
/// over `bb.edn` within a directory.
pub fn find_project_file(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        for name in ["cljrsh.edn", "bb.edn"] {
            let candidate = d.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        dir = d.parent();
    }
    None
}

/// Load and parse the project file at `path`.
pub fn load(path: &Path) -> Result<Project, ProjectError> {
    let src = std::fs::read_to_string(path).map_err(|e| ProjectError::Io(e.to_string()))?;
    let mut parser = cljrs_reader::Parser::new(src, path.display().to_string());
    let form = parser
        .parse_one()
        .map_err(|e| ProjectError::Parse(e.to_string()))?
        .ok_or_else(|| ProjectError::Shape("empty project file".to_string()))?;
    let FormKind::Map(entries) = &form.kind else {
        return Err(ProjectError::Shape("project file must be a map".to_string()));
    };

    let root = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut project = Project {
        root,
        file: path.to_path_buf(),
        paths: Vec::new(),
        min_bb_version: None,
        deps: Vec::new(),
        tasks: Vec::new(),
        init: None,
        pods: Vec::new(),
    };

    for pair in entries.chunks(2) {
        let [k, v] = pair else {
            return Err(ProjectError::Shape("odd number of map entries".to_string()));
        };
        match &k.kind {
            FormKind::Keyword(key) if key == "paths" => {
                let FormKind::Vector(items) = &v.kind else {
                    return Err(ProjectError::Shape(":paths must be a vector".to_string()));
                };
                for item in items {
                    if let FormKind::Str(s) = &item.kind {
                        project.paths.push(s.clone());
                    }
                }
            }
            FormKind::Keyword(key) if key == "min-bb-version" => {
                if let FormKind::Str(s) = &v.kind {
                    project.min_bb_version = Some(s.clone());
                }
            }
            FormKind::Keyword(key) if key == "tasks" => {
                parse_tasks(v, &mut project)?;
            }
            FormKind::Keyword(key) if key == "deps" => {
                parse_deps(v, &mut project)?;
            }
            FormKind::Keyword(key) if key == "pods" => {
                parse_pods(v, &mut project)?;
            }
            // Ignore unknown keys, like bb.
            _ => {}
        }
    }
    Ok(project)
}

/// `:pods {org.babashka/go-sqlite3 {:version "0.1.0"}}`
fn parse_pods(pods_form: &Form, project: &mut Project) -> Result<(), ProjectError> {
    let FormKind::Map(entries) = &pods_form.kind else {
        return Err(ProjectError::Shape(":pods must be a map".to_string()));
    };
    for pair in entries.chunks(2) {
        let [k, v] = pair else {
            return Err(ProjectError::Shape(":pods has an odd entry".to_string()));
        };
        let FormKind::Symbol(name) = &k.kind else {
            return Err(ProjectError::Shape(
                ":pods keys must be qualified symbols".to_string(),
            ));
        };
        let FormKind::Map(coord) = &v.kind else {
            return Err(ProjectError::Shape(format!(
                "pod {name}: coordinate must be a map with :version"
            )));
        };
        let version = coord
            .chunks(2)
            .find(|p| matches!(&p[0].kind, FormKind::Keyword(key) if key == "version"))
            .and_then(|p| match &p[1].kind {
                FormKind::Str(s) => Some(s.clone()),
                _ => None,
            })
            .ok_or_else(|| {
                ProjectError::Shape(format!("pod {name}: missing :version string"))
            })?;
        project.pods.push((name.clone(), version));
    }
    Ok(())
}

fn parse_deps(deps_form: &Form, project: &mut Project) -> Result<(), ProjectError> {
    let FormKind::Map(entries) = &deps_form.kind else {
        return Err(ProjectError::Shape(":deps must be a map".to_string()));
    };
    for pair in entries.chunks(2) {
        let [k, v] = pair else {
            return Err(ProjectError::Shape(":deps has an odd entry".to_string()));
        };
        let FormKind::Symbol(lib) = &k.kind else {
            return Err(ProjectError::Shape(":deps keys must be lib symbols".to_string()));
        };
        let FormKind::Map(coord) = &v.kind else {
            return Err(ProjectError::Shape(format!(
                ":deps value for {lib} must be a coordinate map"
            )));
        };
        let field = |name: &str| -> Option<String> {
            coord.chunks(2).find_map(|p| match (&p[0].kind, p.get(1)) {
                (FormKind::Keyword(key), Some(val)) if key == name => match &val.kind {
                    FormKind::Str(s) => Some(s.clone()),
                    _ => None,
                },
                _ => None,
            })
        };
        let coord = if let Some(root) = field("local/root") {
            DepCoord::Local { root }
        } else if let Some(version) = field("mvn/version") {
            DepCoord::Maven { version }
        } else if let (Some(url), Some(sha)) = (field("git/url"), field("git/sha")) {
            DepCoord::Git { url, sha }
        } else {
            return Err(ProjectError::Shape(format!(
                "dep {lib}: expected :mvn/version, :git/url + :git/sha, or :local/root"
            )));
        };
        project.deps.push((lib.clone(), coord));
    }
    Ok(())
}

/// Resolve every `:deps` entry to source directories: locals directly, git
/// via the `git` CLI into `cache/git/`, maven via [`maven::ensure_deps`].
/// The dep's own `src/` subdirectory is used when present, else its root.
pub fn resolve_deps(project: &Project, cache: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut maven_roots = Vec::new();
    for (lib, coord) in &project.deps {
        match coord {
            DepCoord::Local { root } => {
                let dir = project.root.join(root);
                if !dir.is_dir() {
                    return Err(format!("local dep {lib}: {} not found", dir.display()));
                }
                out.push(source_root(&dir));
            }
            DepCoord::Git { url, sha } => {
                let dir = ensure_git_dep(lib, url, sha, cache)?;
                out.push(source_root(&dir));
            }
            DepCoord::Maven { version } => {
                let (group, artifact) = match lib.split_once('/') {
                    Some((g, a)) => (g.to_string(), a.to_string()),
                    None => (lib.clone(), lib.clone()),
                };
                maven_roots.push(maven::Coord {
                    group,
                    artifact,
                    version: version.clone(),
                });
            }
        }
    }
    out.extend(maven::ensure_deps(&maven_roots, cache)?);
    Ok(out)
}

fn source_root(dir: &Path) -> PathBuf {
    let src = dir.join("src");
    if src.is_dir() { src } else { dir.to_path_buf() }
}

/// Shallow-fetch `url` at `sha` into the cache via the `git` CLI (kept
/// simple: bb shells out for deps too). Idempotent per sha.
fn ensure_git_dep(lib: &str, url: &str, sha: &str, cache: &Path) -> Result<PathBuf, String> {
    let dir = cache.join("git").join(lib.replace('/', "_")).join(sha);
    if dir.join(".git").exists() {
        return Ok(dir);
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let run = |args: &[&str]| -> Result<(), String> {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&dir)
            .output()
            .map_err(|e| format!("git dep {lib}: cannot run git: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "git dep {lib}: git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            ))
        }
    };
    eprintln!("cljrsh: fetching git dep {lib} @ {sha}");
    run(&["init", "-q"])?;
    run(&["remote", "add", "origin", url])?;
    run(&["fetch", "-q", "--depth", "1", "origin", sha])?;
    run(&["checkout", "-q", sha])?;
    Ok(dir)
}

fn parse_tasks(tasks_form: &Form, project: &mut Project) -> Result<(), ProjectError> {
    let FormKind::Map(entries) = &tasks_form.kind else {
        return Err(ProjectError::Shape(":tasks must be a map".to_string()));
    };
    for pair in entries.chunks(2) {
        let [k, v] = pair else {
            return Err(ProjectError::Shape(":tasks has an odd entry".to_string()));
        };
        match &k.kind {
            FormKind::Keyword(key) if key == "init" => {
                project.init = Some(v.clone());
            }
            // :requires / :enter / :leave at the tasks level: later milestone.
            FormKind::Keyword(_) => {}
            FormKind::Symbol(name) => {
                project.tasks.push(parse_task_def(name.clone(), v)?);
            }
            other => {
                return Err(ProjectError::Shape(format!(
                    "task names must be symbols, got {other:?}"
                )));
            }
        }
    }
    Ok(())
}

fn parse_task_def(name: String, v: &Form) -> Result<TaskDef, ProjectError> {
    let mut def = TaskDef {
        name,
        doc: None,
        depends: Vec::new(),
        body: v.clone(),
        private: false,
    };
    if let FormKind::Map(entries) = &v.kind {
        // Map form: {:task expr :depends [...] :doc "..." :private true}
        let mut body: Option<Form> = None;
        for pair in entries.chunks(2) {
            let [k, val] = pair else {
                return Err(ProjectError::Shape(format!(
                    "task {} has an odd map entry",
                    def.name
                )));
            };
            match &k.kind {
                FormKind::Keyword(key) if key == "task" => body = Some(val.clone()),
                FormKind::Keyword(key) if key == "doc" => {
                    if let FormKind::Str(s) = &val.kind {
                        def.doc = Some(s.clone());
                    }
                }
                FormKind::Keyword(key) if key == "depends" => {
                    if let FormKind::Vector(items) = &val.kind {
                        for item in items {
                            if let FormKind::Symbol(s) = &item.kind {
                                def.depends.push(s.clone());
                            }
                        }
                    }
                }
                FormKind::Keyword(key) if key == "private" => {
                    def.private = matches!(val.kind, FormKind::Bool(true));
                }
                _ => {}
            }
        }
        def.body = body.ok_or_else(|| {
            ProjectError::Shape(format!("task {} map is missing :task", def.name))
        })?;
    }
    Ok(def)
}

/// The evaluation order for `target`: its transitive `:depends` in
/// topological order, target last. Depth-first, cycle-detecting.
pub fn task_order<'p>(project: &'p Project, target: &str) -> Result<Vec<&'p TaskDef>, ProjectError> {
    fn visit<'p>(
        project: &'p Project,
        name: &str,
        visiting: &mut Vec<String>,
        done: &mut Vec<String>,
        out: &mut Vec<&'p TaskDef>,
    ) -> Result<(), ProjectError> {
        if done.iter().any(|d| d == name) {
            return Ok(());
        }
        if visiting.iter().any(|v| v == name) {
            return Err(ProjectError::Cycle(name.to_string()));
        }
        let task = project
            .tasks
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| ProjectError::UnknownTask(name.to_string()))?;
        visiting.push(name.to_string());
        for dep in &task.depends {
            visit(project, dep, visiting, done, out)?;
        }
        visiting.pop();
        done.push(name.to_string());
        out.push(task);
        Ok(())
    }

    let mut out = Vec::new();
    visit(
        project,
        target,
        &mut Vec::new(),
        &mut Vec::new(),
        &mut out,
    )?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_project(content: &str) -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bb.edn");
        std::fs::write(&path, content).unwrap();
        let project = load(&path).unwrap();
        (dir, project)
    }

    #[test]
    fn parses_paths_and_tasks_in_order() {
        let (_d, p) = write_project(
            r#"{:paths ["src" "scripts"]
                :tasks {clean (println "clean")
                        build {:task (println "build") :depends [clean] :doc "Build it"}
                        release {:task (println "rel") :depends [build] :private true}}}"#,
        );
        assert_eq!(p.paths, vec!["src", "scripts"]);
        let names: Vec<_> = p.tasks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["clean", "build", "release"]);
        assert_eq!(p.tasks[1].doc.as_deref(), Some("Build it"));
        assert_eq!(p.tasks[1].depends, vec!["clean"]);
        assert!(p.tasks[2].private);
    }

    #[test]
    fn topo_order_resolves_depends() {
        let (_d, p) = write_project(
            r#"{:tasks {a (do 1) b {:task (do 2) :depends [a]} c {:task (do 3) :depends [b a]}}}"#,
        );
        let order: Vec<_> = task_order(&p, "c").unwrap().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[test]
    fn cycle_is_an_error() {
        let (_d, p) = write_project(
            r#"{:tasks {a {:task 1 :depends [b]} b {:task 2 :depends [a]}}}"#,
        );
        assert!(matches!(task_order(&p, "a"), Err(ProjectError::Cycle(_))));
    }

    #[test]
    fn unknown_task_is_an_error() {
        let (_d, p) = write_project(r#"{:tasks {a 1}}"#);
        assert!(matches!(
            task_order(&p, "nope"),
            Err(ProjectError::UnknownTask(_))
        ));
    }

    #[test]
    fn init_is_captured() {
        let (_d, p) = write_project(r#"{:tasks {:init (def base 41) a (inc base)}}"#);
        assert!(p.init.is_some());
        assert_eq!(p.tasks.len(), 1);
    }

    #[test]
    fn discovery_walks_up_and_prefers_cljrsh_edn() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.path().join("bb.edn"), "{}").unwrap();
        assert_eq!(
            find_project_file(&sub).unwrap(),
            dir.path().join("bb.edn")
        );
        std::fs::write(dir.path().join("cljrsh.edn"), "{}").unwrap();
        assert_eq!(
            find_project_file(&sub).unwrap(),
            dir.path().join("cljrsh.edn")
        );
    }
}
