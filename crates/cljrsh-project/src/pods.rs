//! babashka pod-registry resolver: `(pods/load-pod 'org.babashka/foo "1.2.3")`
//! without a local path, and `:pods` entries in bb.edn.
//!
//! Manifests live at
//! `https://raw.githubusercontent.com/babashka/pod-registry/master/manifests/<qualified-name>/<version>/manifest.edn`
//! and list per-platform artifacts (`:os/name` and `:os/arch` are regexes
//! matched against JVM-style platform strings, which we emulate). Artifacts
//! are downloaded and unpacked into
//! `<cache>/pods/repository/<qualified-name>/<version>/`, with the executable
//! name recorded in a `.executable` marker so later runs skip the network
//! entirely.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use cljrs_reader::{Form, FormKind};

const REGISTRY_BASE: &str =
    "https://raw.githubusercontent.com/babashka/pod-registry/master/manifests";

/// JVM `os.name` equivalent, which registry regexes are written against.
fn os_name() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "Mac OS X",
        "windows" => "Windows 10",
        other => other,
    }
}

/// JVM `os.arch` equivalent (linux JVMs report x86_64 as `amd64`).
fn os_arch() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "x86_64") => "x86_64",
        (_, "x86_64") => "amd64",
        (_, "aarch64") => "aarch64",
        (_, other) => other,
    }
}

/// Resolve a registry pod to a runnable executable path, downloading and
/// unpacking on first use. Network work runs on a dedicated OS thread so this
/// is safe to call from inside the async driver (reqwest's blocking client
/// owns a tokio runtime that must not be created or dropped in async context).
pub fn ensure_registry_pod(name: &str, version: &str, cache: &Path) -> Result<PathBuf, String> {
    if name.contains("..") || version.contains("..") || version.is_empty() {
        return Err(format!("invalid pod coordinate {name} {version}"));
    }
    let dir = cache.join("pods/repository").join(name).join(version);
    let marker = dir.join(".executable");
    if let Ok(exe) = std::fs::read_to_string(&marker) {
        let path = dir.join(exe.trim());
        if path.is_file() {
            return Ok(path);
        }
    }

    let name = name.to_string();
    let version = version.to_string();
    std::thread::Builder::new()
        .name("cljrsh-pod-fetch".into())
        .spawn(move || fetch_pod(&name, &version, &dir))
        .map_err(|e| format!("cannot spawn pod fetch thread: {e}"))?
        .join()
        .map_err(|_| "pod fetch thread panicked".to_string())?
}

/// Blocking fetch + unpack (runs on its own thread).
fn fetch_pod(name: &str, version: &str, dir: &Path) -> Result<PathBuf, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("cljrsh")
        .build()
        .map_err(|e| format!("http client: {e}"))?;

    let manifest_url = format!("{REGISTRY_BASE}/{name}/{version}/manifest.edn");
    let resp = client
        .get(&manifest_url)
        .send()
        .map_err(|e| format!("fetching pod manifest {manifest_url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "pod {name} {version} not found in the pod registry ({} from {manifest_url})",
            resp.status()
        ));
    }
    let manifest_src = resp
        .text()
        .map_err(|e| format!("reading pod manifest: {e}"))?;
    let artifact = select_artifact(&manifest_src)?.ok_or_else(|| {
        format!(
            "pod {name} {version} has no artifact for {} / {}",
            os_name(),
            os_arch()
        )
    })?;

    let bytes = client
        .get(&artifact.url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("downloading pod artifact {}: {e}", artifact.url))?
        .bytes()
        .map_err(|e| format!("downloading pod artifact: {e}"))?;

    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    if artifact.url.ends_with(".zip") {
        unzip_into(&bytes, dir)?;
    } else if artifact.url.ends_with(".tar.gz") || artifact.url.ends_with(".tgz") {
        untar_into(&bytes, dir)?;
    } else {
        // A raw executable.
        std::fs::write(dir.join(&artifact.executable), &bytes)
            .map_err(|e| format!("writing pod executable: {e}"))?;
    }

    let exe = dir.join(&artifact.executable);
    if !exe.is_file() {
        return Err(format!(
            "pod artifact did not contain expected executable {}",
            artifact.executable
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod pod executable: {e}"))?;
    }
    std::fs::write(dir.join(".executable"), &artifact.executable)
        .map_err(|e| format!("writing pod marker: {e}"))?;
    Ok(exe)
}

struct Artifact {
    url: String,
    executable: String,
}

/// Parse the manifest and pick the first artifact whose `:os/name` and
/// `:os/arch` regexes match this platform.
fn select_artifact(manifest_src: &str) -> Result<Option<Artifact>, String> {
    let mut parser = cljrs_reader::Parser::new(manifest_src.to_string(), "manifest.edn".into());
    let form = parser
        .parse_one()
        .map_err(|e| format!("parsing pod manifest: {e}"))?
        .ok_or("empty pod manifest")?;
    let FormKind::Map(entries) = &form.kind else {
        return Err("pod manifest must be a map".into());
    };
    let artifacts = match map_get(entries, "pod/artifacts") {
        Some(Form {
            kind: FormKind::Vector(items),
            ..
        }) => items,
        _ => return Err("pod manifest has no :pod/artifacts".into()),
    };

    for item in artifacts {
        let FormKind::Map(fields) = &item.kind else {
            continue;
        };
        let name_re = str_field(fields, "os/name").unwrap_or_else(|| ".*".into());
        let arch_re = str_field(fields, "os/arch").unwrap_or_else(|| ".*".into());
        if !whole_match(&name_re, os_name())? || !whole_match(&arch_re, os_arch())? {
            continue;
        }
        let (Some(url), Some(executable)) = (
            str_field(fields, "artifact/url"),
            str_field(fields, "artifact/executable"),
        ) else {
            continue;
        };
        return Ok(Some(Artifact { url, executable }));
    }
    Ok(None)
}

fn whole_match(pattern: &str, s: &str) -> Result<bool, String> {
    let re = regex::Regex::new(&format!("^(?:{pattern})$"))
        .map_err(|e| format!("bad regex in pod manifest ({pattern}): {e}"))?;
    Ok(re.is_match(s))
}

fn map_get<'a>(entries: &'a [Form], key: &str) -> Option<&'a Form> {
    entries
        .chunks(2)
        .find(|pair| matches!(&pair[0].kind, FormKind::Keyword(k) if k == key))
        .map(|pair| &pair[1])
}

fn str_field(entries: &[Form], key: &str) -> Option<String> {
    match &map_get(entries, key)?.kind {
        FormKind::Str(s) => Some(s.clone()),
        _ => None,
    }
}

fn unzip_into(bytes: &[u8], dir: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("reading pod zip: {e}"))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("reading pod zip entry: {e}"))?;
        let Some(rel) = entry.enclosed_name() else {
            continue; // path traversal — skip
        };
        let out = dir.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut content = Vec::new();
        entry
            .read_to_end(&mut content)
            .map_err(|e| format!("extracting pod zip: {e}"))?;
        std::fs::write(&out, content).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn untar_into(bytes: &[u8], dir: &Path) -> Result<(), String> {
    let gz = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    tar::Archive::new(gz)
        .unpack(dir)
        .map_err(|e| format!("extracting pod tarball: {e}"))
}

/// Default cljrsh cache directory: `$XDG_CACHE_HOME/cljrsh` or
/// `~/.cache/cljrsh`.
pub fn default_cache_dir() -> PathBuf {
    std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|_| PathBuf::from(".cljrsh-cache"))
        .join("cljrsh")
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
{:pod/name org.example/demo
 :pod/version "1.0.0"
 :pod/artifacts
 [{:os/name "NoSuchOS.*" :os/arch ".*"
   :artifact/url "https://example.com/never.zip"
   :artifact/executable "never"}
  {:os/name ".*" :os/arch ".*"
   :artifact/url "https://example.com/demo.zip"
   :artifact/executable "demo"}]}
"#;

    #[test]
    fn selects_first_matching_artifact() {
        let a = select_artifact(MANIFEST).unwrap().expect("artifact");
        assert_eq!(a.url, "https://example.com/demo.zip");
        assert_eq!(a.executable, "demo");
    }

    #[test]
    fn no_match_yields_none() {
        let manifest = r#"{:pod/artifacts [{:os/name "BeOS" :os/arch "vax"
                            :artifact/url "u" :artifact/executable "e"}]}"#;
        assert!(select_artifact(manifest).unwrap().is_none());
    }

    #[test]
    fn platform_strings_look_jvm_like() {
        assert!(["Linux", "Mac OS X", "Windows 10"].contains(&os_name()));
        assert!(["amd64", "x86_64", "aarch64"].contains(&os_arch()));
    }
}
