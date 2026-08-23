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

/// Paths registered by `fs/delete-on-exit`, removed by the hosting binary
/// on clean shutdown (see cljrsh's main).
static EXIT_DELETES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub fn register_exit_delete(path: &str) {
    EXIT_DELETES.lock().unwrap().push(path.to_string());
}

/// Best-effort removal of every delete-on-exit registration.
pub fn run_exit_deletes() {
    for path in EXIT_DELETES.lock().unwrap().drain(..) {
        let p = Path::new(&path);
        let _ = if p.is_dir() {
            std::fs::remove_dir_all(p)
        } else {
            std::fs::remove_file(p)
        };
    }
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
    def1(registry, "read-link", |p| {
        let target = std::fs::read_link(p).map_err(|e| io_err("read-link", p, e))?;
        Ok(Value::string(target.display().to_string()))
    });
    def1(registry, "create-dir", |p| {
        std::fs::create_dir(p).map_err(|e| io_err("create-dir", p, e))?;
        Ok(Value::string(p.to_string()))
    });
    def1(registry, "create-file", |p| {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(p)
            .map_err(|e| io_err("create-file", p, e))?;
        Ok(Value::string(p.to_string()))
    });
    def1(registry, "executable?", |p| {
        use std::os::unix::fs::PermissionsExt;
        Ok(Value::Bool(
            std::fs::metadata(p)
                .map(|md| md.permissions().mode() & 0o111 != 0)
                .unwrap_or(false),
        ))
    });
    def1(registry, "writable?", |p| {
        Ok(Value::Bool(
            std::fs::metadata(p)
                .map(|md| !md.permissions().readonly())
                .unwrap_or(false),
        ))
    });
    def1(registry, "hidden?", |p| {
        Ok(Value::Bool(
            Path::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().starts_with('.'))
                .unwrap_or(false),
        ))
    });
    def1(registry, "creation-time-millis", |p| {
        let md = std::fs::metadata(p).map_err(|e| io_err("creation-time-millis", p, e))?;
        Ok(match md.created().ok().and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH).ok()
        }) {
            Some(d) => Value::Long(d.as_millis() as i64),
            None => Value::Nil,
        })
    });
    def1(registry, "normalize", |p| {
        // Lexical normalization (no IO): resolve `.` and non-leading `..`.
        let mut out: Vec<std::path::Component> = Vec::new();
        for c in Path::new(p).components() {
            match c {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => match out.last() {
                    Some(std::path::Component::Normal(_)) => {
                        out.pop();
                    }
                    Some(std::path::Component::RootDir) => {}
                    _ => out.push(c),
                },
                other => out.push(other),
            }
        }
        let joined: std::path::PathBuf = out.iter().collect();
        Ok(Value::string(joined.display().to_string()))
    });
    registry.define(
        "cljrsh.fs/create-sym-link",
        wrap_fn2(
            "cljrsh.fs/create-sym-link",
            |link: Value, target: Value| -> Result<Value, String> {
                let link = str_arg(&link, "link path")?;
                let target = str_arg(&target, "target path")?;
                std::os::unix::fs::symlink(&target, &link)
                    .map_err(|e| io_err("create-sym-link", &link, e))?;
                Ok(Value::string(link))
            },
        ),
    );
    registry.define(
        "cljrsh.fs/create-link",
        wrap_fn2(
            "cljrsh.fs/create-link",
            |link: Value, target: Value| -> Result<Value, String> {
                let link = str_arg(&link, "link path")?;
                let target = str_arg(&target, "target path")?;
                std::fs::hard_link(&target, &link)
                    .map_err(|e| io_err("create-link", &link, e))?;
                Ok(Value::string(link))
            },
        ),
    );
    registry.define(
        "cljrsh.fs/set-unix-mode",
        wrap_fn2(
            "cljrsh.fs/set-unix-mode",
            |path: Value, mode: Value| -> Result<Value, String> {
                use std::os::unix::fs::PermissionsExt;
                let path = str_arg(&path, "path")?;
                let Value::Long(mode) = mode else {
                    return Err("set-unix-mode: mode must be an integer".into());
                };
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode as u32))
                    .map_err(|e| io_err("set-unix-mode", &path, e))?;
                Ok(Value::string(path))
            },
        ),
    );
    registry.define(
        "cljrsh.fs/relativize",
        wrap_fn2(
            "cljrsh.fs/relativize",
            |base: Value, other: Value| -> Result<Value, String> {
                let base = str_arg(&base, "base path")?;
                let other = str_arg(&other, "other path")?;
                let base_parts: Vec<_> = Path::new(&base).components().collect();
                let other_parts: Vec<_> = Path::new(&other).components().collect();
                let common = base_parts
                    .iter()
                    .zip(other_parts.iter())
                    .take_while(|(a, b)| a == b)
                    .count();
                let mut rel = std::path::PathBuf::new();
                for _ in common..base_parts.len() {
                    rel.push("..");
                }
                for c in &other_parts[common..] {
                    rel.push(c);
                }
                Ok(Value::string(rel.display().to_string()))
            },
        ),
    );
    registry.define(
        "cljrsh.fs/create-temp-file",
        wrap_fn_variadic(
            "cljrsh.fs/create-temp-file",
            0,
            |_args: &[Value]| -> Result<Value, String> {
                let file = tempfile::Builder::new()
                    .prefix("cljrsh-")
                    .tempfile()
                    .map_err(|e| format!("create-temp-file: {e}"))?;
                let (_, path) = file
                    .keep()
                    .map_err(|e| format!("create-temp-file: {e}"))?;
                Ok(Value::string(path.display().to_string()))
            },
        ),
    );
    def1(registry, "delete-on-exit", |p| {
        crate::fs::register_exit_delete(p);
        Ok(Value::string(p.to_string()))
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

    def1(registry, "which-all", |p| {
        Ok(string_vec(
            which::which_all(p)
                .map(|found| found.map(|f| f.display().to_string()).collect())
                .unwrap_or_default(),
        ))
    });
    def1(registry, "read-bytes", |p| {
        let bytes = std::fs::read(p).map_err(|e| io_err("read-bytes", p, e))?;
        let signed: Vec<i8> = bytes.into_iter().map(|b| b as i8).collect();
        Ok(Value::ByteArray(GcPtr::new(std::sync::Mutex::new(signed))))
    });
    registry.define(
        "cljrsh.fs/write-bytes",
        wrap_fn2(
            "cljrsh.fs/write-bytes",
            |path: Value, bytes: Value| -> Result<Value, String> {
                let path = str_arg(&path, "path")?;
                let Value::ByteArray(a) = bytes else {
                    return Err(format!(
                        "write-bytes: expected a byte-array, got {}",
                        bytes.type_name()
                    ));
                };
                let unsigned: Vec<u8> =
                    a.get().lock().unwrap().iter().map(|b| *b as u8).collect();
                std::fs::write(&path, unsigned).map_err(|e| io_err("write-bytes", &path, e))?;
                Ok(Value::string(path))
            },
        ),
    );

    // (gzip src) / (gzip src out-file) — default out is src + ".gz".
    registry.define(
        "cljrsh.fs/gzip",
        wrap_fn_variadic(
            "cljrsh.fs/gzip",
            1,
            |args: &[Value]| -> Result<Value, String> {
                let src = str_arg(&args[0], "source")?;
                let out = match args.get(1) {
                    Some(v) => str_arg(v, "out-file")?,
                    None => format!("{src}.gz"),
                };
                let mut input =
                    std::fs::File::open(&src).map_err(|e| io_err("gzip", &src, e))?;
                let output =
                    std::fs::File::create(&out).map_err(|e| io_err("gzip", &out, e))?;
                let mut enc =
                    flate2::write::GzEncoder::new(output, flate2::Compression::default());
                std::io::copy(&mut input, &mut enc).map_err(|e| io_err("gzip", &src, e))?;
                enc.finish().map_err(|e| io_err("gzip", &out, e))?;
                Ok(Value::string(out))
            },
        ),
    );
    // (gunzip src) / (gunzip src out-file) — default out strips the ".gz".
    registry.define(
        "cljrsh.fs/gunzip",
        wrap_fn_variadic(
            "cljrsh.fs/gunzip",
            1,
            |args: &[Value]| -> Result<Value, String> {
                let src = str_arg(&args[0], "source")?;
                let out = match args.get(1) {
                    Some(v) => str_arg(v, "out-file")?,
                    None => src.strip_suffix(".gz").map(str::to_string).ok_or_else(
                        || format!("gunzip {src}: no .gz suffix; pass an out-file"),
                    )?,
                };
                let input = std::fs::File::open(&src).map_err(|e| io_err("gunzip", &src, e))?;
                let mut dec = flate2::read::GzDecoder::new(input);
                let mut output =
                    std::fs::File::create(&out).map_err(|e| io_err("gunzip", &out, e))?;
                std::io::copy(&mut dec, &mut output).map_err(|e| io_err("gunzip", &src, e))?;
                Ok(Value::string(out))
            },
        ),
    );
    // (zip zip-file paths root) — each path (file or tree) is stored with
    // entry names relative to root; a path outside root is an error.
    registry.define(
        "cljrsh.fs/zip",
        wrap_fn_variadic(
            "cljrsh.fs/zip",
            3,
            |args: &[Value]| -> Result<Value, String> {
                let zip_file = str_arg(&args[0], "zip-file")?;
                let Value::Vector(paths) = &args[1] else {
                    return Err("zip: paths must be a vector".into());
                };
                let root = PathBuf::from(str_arg(&args[2], "root")?);
                let entry_name = |p: &Path| -> Result<String, String> {
                    p.strip_prefix(&root)
                        .map(|rel| rel.display().to_string())
                        .map_err(|_| {
                            format!("zip: {} is not under root {}", p.display(), root.display())
                        })
                };
                let output = std::fs::File::create(&zip_file)
                    .map_err(|e| io_err("zip", &zip_file, e))?;
                let mut zw = zip::ZipWriter::new(output);
                let opts = zip::write::SimpleFileOptions::default();
                let zerr = |e: zip::result::ZipError| format!("zip {zip_file}: {e}");
                for v in paths.get().iter() {
                    let top = str_arg(v, "path")?;
                    for entry in walkdir::WalkDir::new(&top)
                        .sort_by_file_name()
                        .into_iter()
                        .filter_map(Result::ok)
                    {
                        let name = entry_name(entry.path())?;
                        if entry.file_type().is_dir() {
                            zw.add_directory(name, opts).map_err(zerr)?;
                        } else {
                            zw.start_file(name, opts).map_err(zerr)?;
                            let mut input = std::fs::File::open(entry.path())
                                .map_err(|e| io_err("zip", &top, e))?;
                            std::io::copy(&mut input, &mut zw)
                                .map_err(|e| io_err("zip", &top, e))?;
                        }
                    }
                }
                zw.finish().map_err(zerr)?;
                Ok(Value::string(zip_file))
            },
        ),
    );
    // (unzip zip-file dest) — extraction is sanitized by the zip crate, so
    // entries cannot escape dest (zip-slip).
    registry.define(
        "cljrsh.fs/unzip",
        wrap_fn2(
            "cljrsh.fs/unzip",
            |zip_file: Value, dest: Value| -> Result<Value, String> {
                let zip_file = str_arg(&zip_file, "zip-file")?;
                let dest = str_arg(&dest, "dest")?;
                let input =
                    std::fs::File::open(&zip_file).map_err(|e| io_err("unzip", &zip_file, e))?;
                let mut archive = zip::ZipArchive::new(input)
                    .map_err(|e| format!("unzip {zip_file}: {e}"))?;
                archive
                    .extract(&dest)
                    .map_err(|e| format!("unzip {zip_file}: {e}"))?;
                Ok(Value::string(dest))
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
