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
    /// `:tasks`, in source order.
    pub tasks: Vec<TaskDef>,
    /// `:tasks`' `:init` form, evaluated once before any task body.
    pub init: Option<Form>,
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
        tasks: Vec::new(),
        init: None,
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
            // :deps, :pods — later milestones; ignore unknown keys like bb.
            _ => {}
        }
    }
    Ok(project)
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
