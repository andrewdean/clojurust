//! `cljrsh.fs` — native filesystem operations. Paths are plain strings in
//! and out (no Path object type); babashka.fs's Path-returning API is
//! papered over in the compat veneer.

use std::path::{Path, PathBuf};

use cljrs_gc::GcPtr;
use cljrs_interop::{Registry, wrap_fn1, wrap_fn2, wrap_fn_variadic};
use cljrs_value::{PersistentVector, Value};

fn str_arg(v: &Value, what: &str) -> Result<String, String> {
    match v {
        Value::Str(s) => Ok(s.get().to_string()),
        other => Err(format!("{what} must be a string, got {}", other.type_name())),
    }
}

fn string_vec(items: Vec<String>) -> Value {
    Value::Vector(GcPtr::new(PersistentVector::from_iter(
        items.into_iter().map(Value::string),
    )))
}

fn io_err(op: &str, path: &str, e: impl std::fmt::Display) -> String {
    format!("{op} {path}: {e}")
}

pub fn register(registry: &mut Registry) {
    let ns = "cljrsh.fs";
    let def1 = |registry: &mut Registry,
                name: &str,
                f: fn(&str) -> Result<Value, String>| {
        let qualified = format!("{ns}/{name}");
        registry.define(
            &qualified,
            wrap_fn1(qualified.clone(), move |v: Value| -> Result<Value, String> {
                f(&str_arg(&v, "path")?)
            }),
        );
    };

    def1(registry, "exists?", |p| {
        Ok(Value::Bool(Path::new(p).exists()))
    });
    def1(registry, "directory?", |p| {
        Ok(Value::Bool(Path::new(p).is_dir()))
    });
    def1(registry, "regular-file?", |p| {
        Ok(Value::Bool(Path::new(p).is_file()))
    });
    def1(registry, "sym-link?", |p| {
        Ok(Value::Bool(Path::new(p).is_symlink()))
    });
    def1(registry, "readable?", |p| {
        Ok(Value::Bool(std::fs::metadata(p).is_ok()))
    });
    def1(registry, "size", |p| {
        let md = std::fs::metadata(p).map_err(|e| io_err("size", p, e))?;
        Ok(Value::Long(md.len() as i64))
    });
    def1(registry, "unix-mode", |p| {
        // Permission bits only (mode & 0o7777) — the `stat -c %a` idiom.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let md = std::fs::metadata(p).map_err(|e| io_err("unix-mode", p, e))?;
            Ok(Value::Long((md.permissions().mode() & 0o7777) as i64))
        }
        #[cfg(not(unix))]
        {
            let _ = p;
            Err("unix-mode is only available on unix hosts".to_string())
        }
    });
    def1(registry, "modified-time-millis", |p| {
        let md = std::fs::metadata(p).map_err(|e| io_err("modified-time", p, e))?;
        let t = md
            .modified()
            .map_err(|e| io_err("modified-time", p, e))?
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map_err(|e| io_err("modified-time", p, e))?;
        Ok(Value::Long(t.as_millis() as i64))
    });
    def1(registry, "list-dir", |p| {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(p).map_err(|e| io_err("list-dir", p, e))? {
            let entry = entry.map_err(|e| io_err("list-dir", p, e))?;
            out.push(entry.path().display().to_string());
        }
        out.sort();
        Ok(string_vec(out))
    });
    def1(registry, "create-dirs", |p| {
        std::fs::create_dir_all(p).map_err(|e| io_err("create-dirs", p, e))?;
        Ok(Value::string(p.to_string()))
    });
    def1(registry, "delete", |p| {
        let path = Path::new(p);
        let r = if path.is_dir() {
            std::fs::remove_dir(path)
        } else {
            std::fs::remove_file(path)
        };
        r.map_err(|e| io_err("delete", p, e))?;
        Ok(Value::Nil)
    });
    def1(registry, "delete-tree", |p| {
        if Path::new(p).exists() {
            std::fs::remove_dir_all(p).map_err(|e| io_err("delete-tree", p, e))?;
        }
        Ok(Value::Nil)
    });
    def1(registry, "absolutize", |p| {
        let path = Path::new(p);
        let abs: PathBuf = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| io_err("absolutize", p, e))?
                .join(path)
        };
        Ok(Value::string(abs.display().to_string()))
    });
    def1(registry, "canonicalize", |p| {
        let c = std::fs::canonicalize(p).map_err(|e| io_err("canonicalize", p, e))?;
        Ok(Value::string(c.display().to_string()))
    });
    def1(registry, "file-name", |p| {
        Ok(Path::new(p)
            .file_name()
            .map(|n| Value::string(n.to_string_lossy().into_owned()))
            .unwrap_or(Value::Nil))
    });
    def1(registry, "parent", |p| {
        Ok(Path::new(p)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|n| Value::string(n.display().to_string()))
            .unwrap_or(Value::Nil))
    });
    def1(registry, "extension", |p| {
        Ok(Path::new(p)
            .extension()
            .map(|n| Value::string(n.to_string_lossy().into_owned()))
            .unwrap_or(Value::Nil))
    });
    def1(registry, "which", |p| {
        Ok(which::which(p)
            .map(|found| Value::string(found.display().to_string()))
            .unwrap_or(Value::Nil))
    });

    registry.define(
        "cljrsh.fs/copy",
        wrap_fn2(
            "cljrsh.fs/copy",
            |from: Value, to: Value| -> Result<Value, String> {
                let (from, to) = (str_arg(&from, "from")?, str_arg(&to, "to")?);
                std::fs::copy(&from, &to).map_err(|e| io_err("copy", &from, e))?;
                Ok(Value::string(to))
            },
        ),
    );
    registry.define(
        "cljrsh.fs/move",
        wrap_fn2(
            "cljrsh.fs/move",
            |from: Value, to: Value| -> Result<Value, String> {
                let (from, to) = (str_arg(&from, "from")?, str_arg(&to, "to")?);
                std::fs::rename(&from, &to).map_err(|e| io_err("move", &from, e))?;
                Ok(Value::string(to))
            },
        ),
    );
    registry.define(
        "cljrsh.fs/copy-tree",
        wrap_fn2(
            "cljrsh.fs/copy-tree",
            |from: Value, to: Value| -> Result<Value, String> {
                let (from, to) = (str_arg(&from, "from")?, str_arg(&to, "to")?);
                copy_tree(Path::new(&from), Path::new(&to))
                    .map_err(|e| io_err("copy-tree", &from, e))?;
                Ok(Value::string(to))
            },
        ),
    );
    registry.define(
        "cljrsh.fs/temp-dir",
        wrap_fn_variadic(
            "cljrsh.fs/temp-dir",
            0,
            |_args: &[Value]| -> Result<Value, String> {
                Ok(Value::string(std::env::temp_dir().display().to_string()))
            },
        ),
    );
    registry.define(
        "cljrsh.fs/create-temp-dir",
        wrap_fn_variadic(
            "cljrsh.fs/create-temp-dir",
            0,
            |_args: &[Value]| -> Result<Value, String> {
                let dir = tempfile::Builder::new()
                    .prefix("cljrsh-")
                    .tempdir()
                    .map_err(|e| format!("create-temp-dir: {e}"))?;
                // Keep the directory: scripts manage cleanup themselves.
                Ok(Value::string(dir.keep().display().to_string()))
            },
        ),
    );
    registry.define(
        "cljrsh.fs/cwd",
        wrap_fn_variadic("cljrsh.fs/cwd", 0, |_args: &[Value]| -> Result<Value, String> {
            std::env::current_dir()
                .map(|p| Value::string(p.display().to_string()))
                .map_err(|e| format!("cwd: {e}"))
        }),
    );
    registry.define(
        "cljrsh.fs/home",
        wrap_fn_variadic("cljrsh.fs/home", 0, |_args: &[Value]| -> Result<Value, String> {
            Ok(std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .map(Value::string)
                .unwrap_or(Value::Nil))
        }),
    );

    // (glob root pattern) — relative glob under root, returns matching paths
    // (files and dirs), root-relative patterns like "**/*.clj".
    registry.define(
        "cljrsh.fs/glob",
        wrap_fn2(
            "cljrsh.fs/glob",
            |root: Value, pattern: Value| -> Result<Value, String> {
                let root = str_arg(&root, "root")?;
                let pattern = str_arg(&pattern, "pattern")?;
                let glob = globset::GlobBuilder::new(&pattern)
                    .literal_separator(true)
                    .build()
                    .map_err(|e| format!("glob pattern {pattern:?}: {e}"))?
                    .compile_matcher();
                let mut out = Vec::new();
                for entry in walkdir::WalkDir::new(&root)
                    .min_depth(1)
                    .into_iter()
                    .filter_map(Result::ok)
                {
                    let rel = entry
                        .path()
                        .strip_prefix(&root)
                        .unwrap_or(entry.path());
                    if glob.is_match(rel) {
                        out.push(entry.path().display().to_string());
                    }
                }
                out.sort();
                Ok(string_vec(out))
            },
        ),
    );

    // (walk root) / (walk root {:skip-dirs ["node_modules" ...]
    //                            :files-only true})
    // — every path under root, depth-first. :skip-dirs prunes matching
    // directory NAMES during traversal (never descending), which is the
    // difference between milliseconds and a minute on repos with
    // node_modules/target trees.
    registry.define(
        "cljrsh.fs/walk",
        wrap_fn_variadic(
            "cljrsh.fs/walk",
            1,
            |args: &[Value]| -> Result<Value, String> {
                let root = str_arg(&args[0], "root")?;
                let mut skip: std::collections::HashSet<String> = Default::default();
                let mut files_only = false;
                if let Some(Value::Map(m)) = args.get(1) {
                    if let Some(Value::Vector(v)) =
                        m.get(&Value::keyword(cljrs_value::Keyword::simple("skip-dirs")))
                    {
                        for x in v.get().iter() {
                            if let Value::Str(s) = x {
                                skip.insert(s.get().to_string());
                            }
                        }
                    }
                    if let Some(Value::Bool(true)) =
                        m.get(&Value::keyword(cljrs_value::Keyword::simple("files-only")))
                    {
                        files_only = true;
                    }
                }
                let mut out = Vec::new();
                let walker = walkdir::WalkDir::new(&root).min_depth(1).into_iter();
                for entry in walker
                    .filter_entry(|e| {
                        !(e.file_type().is_dir()
                            && e.file_name()
                                .to_str()
                                .is_some_and(|n| skip.contains(n)))
                    })
                    .filter_map(Result::ok)
                {
                    if files_only && !entry.file_type().is_file() {
                        continue;
                    }
                    out.push(entry.path().display().to_string());
                }
                out.sort();
                Ok(string_vec(out))
            },
        ),
    );
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in walkdir::WalkDir::new(from).min_depth(1) {
        let entry = entry.map_err(std::io::Error::other)?;
        let rel = entry
            .path()
            .strip_prefix(from)
            .map_err(std::io::Error::other)?;
        let target = to.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
