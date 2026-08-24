//! Minimal Maven/Clojars artifact fetcher — deliberately the smallest useful
//! thing (see the cljrsh plan): Clojars first then Maven Central, pom + jar
//! GET into a standard-layout cache, **naive transitivity** (compile-scope,
//! non-optional, first-declared-version-wins, same-pom property
//! interpolation only), hard error on version ranges, and jars unzipped once
//! so the interpreter's `require` sees plain source directories. No
//! snapshots, classifiers, mirrors, or full mediation.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

/// `group:artifact` → resolved coordinates, breadth-first.
#[derive(Debug, Clone, PartialEq)]
pub struct Coord {
    pub group: String,
    pub artifact: String,
    pub version: String,
}

impl Coord {
    fn ga(&self) -> String {
        format!("{}:{}", self.group, self.artifact)
    }
    fn rel(&self, ext: &str) -> String {
        format!(
            "{}/{}/{}/{}-{}.{}",
            self.group.replace('.', "/"),
            self.artifact,
            self.version,
            self.artifact,
            self.version,
            ext
        )
    }
}

const REPOS: &[&str] = &[
    "https://repo.clojars.org",
    "https://repo.maven.apache.org/maven2",
];

/// cljrsh ships clojure.core itself — never fetch the JVM Clojure artifacts.
const EXCLUDED: &[&str] = &[
    "org.clojure:clojure",
    "org.clojure:spec.alpha",
    "org.clojure:core.specs.alpha",
];

/// Resolve `roots` (and their naive transitive closure) into extracted source
/// directories, downloading anything missing into `cache`.
pub fn ensure_deps(roots: &[Coord], cache: &Path) -> Result<Vec<PathBuf>, String> {
    let mvn = cache.join("mvn");
    let extracted = cache.join("mvn-extracted");
    let mut queue: Vec<Coord> = roots.to_vec();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();

    while !queue.is_empty() {
        let mut next = Vec::new();
        for coord in queue {
            if EXCLUDED.contains(&coord.ga().as_str()) || !seen.insert(coord.ga()) {
                continue;
            }
            let jar = fetch(&coord, "jar", &mvn)?;
            let pom = fetch(&coord, "pom", &mvn)?;
            let dir = extract_jar(&coord, &jar, &extracted)?;
            out.push(dir);
            let pom_src = std::fs::read_to_string(&pom)
                .map_err(|e| format!("reading {}: {e}", pom.display()))?;
            next.extend(pom_dependencies(&coord, &pom_src)?);
        }
        queue = next;
    }
    Ok(out)
}

/// Download (if missing) one artifact file; returns its cache path.
fn fetch(coord: &Coord, ext: &str, mvn_cache: &Path) -> Result<PathBuf, String> {
    let rel = coord.rel(ext);
    let target = mvn_cache.join(&rel);
    if target.is_file() {
        return Ok(target);
    }
    std::fs::create_dir_all(target.parent().unwrap())
        .map_err(|e| format!("creating {}: {e}", target.display()))?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    for repo in REPOS {
        let url = format!("{repo}/{rel}");
        match client.get(&url).send() {
            Ok(resp) if resp.status().is_success() => {
                let bytes = resp
                    .bytes()
                    .map_err(|e| format!("downloading {url}: {e}"))?;
                eprintln!("cljrsh: downloaded {url}");
                let tmp = target.with_extension(format!("{ext}.part"));
                std::fs::File::create(&tmp)
                    .and_then(|mut f| f.write_all(&bytes))
                    .map_err(|e| format!("writing {}: {e}", tmp.display()))?;
                std::fs::rename(&tmp, &target)
                    .map_err(|e| format!("renaming {}: {e}", target.display()))?;
                return Ok(target);
            }
            _ => continue,
        }
    }
    Err(format!(
        "artifact {}:{}:{} ({ext}) not found on Clojars or Maven Central",
        coord.group, coord.artifact, coord.version
    ))
}

/// Unzip the jar's entries (skipping META-INF) into a per-artifact directory.
fn extract_jar(coord: &Coord, jar: &Path, extracted: &Path) -> Result<PathBuf, String> {
    let dir = extracted
        .join(coord.group.replace('.', "/"))
        .join(&coord.artifact)
        .join(&coord.version);
    let marker = dir.join(".cljrsh-extracted");
    if marker.is_file() {
        return Ok(dir);
    }
    let file = std::fs::File::open(jar).map_err(|e| format!("opening {}: {e}", jar.display()))?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| format!("bad jar {}: {e}", jar.display()))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let Some(name) = entry.enclosed_name() else {
            continue; // path traversal — skip
        };
        if name.starts_with("META-INF") || entry.is_dir() {
            continue;
        }
        let target = dir.join(&name);
        std::fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
        let mut out =
            std::fs::File::create(&target).map_err(|e| format!("{}: {e}", target.display()))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
    }
    std::fs::write(&marker, b"").map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Compile-scope, non-optional dependencies of a pom, with `${...}`
/// interpolation from the same pom's `<properties>` and project coords.
/// Version ranges are a hard error; a dependency with no resolvable version
/// is skipped with a warning.
fn pom_dependencies(owner: &Coord, pom: &str) -> Result<Vec<Coord>, String> {
    // The <dependencyManagement> section pins versions without declaring
    // dependencies; strip it so its entries aren't fetched, but keep it for
    // version lookups.
    let (managed, body) = match (
        pom.find("<dependencyManagement>"),
        pom.find("</dependencyManagement>"),
    ) {
        (Some(a), Some(b)) if b > a => {
            let managed = &pom[a..b];
            let mut body = String::with_capacity(pom.len());
            body.push_str(&pom[..a]);
            body.push_str(&pom[b..]);
            (parse_dep_blocks(managed), body)
        }
        _ => (Vec::new(), pom.to_string()),
    };

    let props = parse_properties(pom);
    let interp = |s: &str| -> Option<String> {
        if !s.contains("${") {
            return Some(s.to_string());
        }
        let key = s.strip_prefix("${")?.strip_suffix('}')?;
        match key {
            "project.version" | "version" => Some(owner.version.clone()),
            "project.groupId" | "groupId" => Some(owner.group.clone()),
            _ => props.get(key).cloned(),
        }
    };

    let mut out = Vec::new();
    for dep in parse_dep_blocks(&body) {
        let scope = dep.scope.as_deref().unwrap_or("compile");
        if scope != "compile" || dep.optional {
            continue;
        }
        let (Some(group), Some(artifact)) = (
            dep.group.as_deref().and_then(&interp),
            dep.artifact.as_deref().and_then(&interp),
        ) else {
            continue;
        };
        let version = dep.version.as_deref().and_then(&interp).or_else(|| {
            managed
                .iter()
                .find(|m| {
                    m.group.as_deref() == Some(group.as_str())
                        && m.artifact.as_deref() == Some(artifact.as_str())
                })
                .and_then(|m| m.version.as_deref().and_then(&interp))
        });
        let Some(version) = version else {
            eprintln!(
                "cljrsh: warning: skipping dep {group}:{artifact} of {}:{} (no resolvable version; parent poms are not consulted)",
                owner.group, owner.artifact
            );
            continue;
        };
        if version.contains('[') || version.contains('(') {
            return Err(format!(
                "dep {group}:{artifact} of {}:{} uses a version range ({version}); pin an explicit :mvn/version",
                owner.group, owner.artifact
            ));
        }
        out.push(Coord {
            group,
            artifact,
            version,
        });
    }
    Ok(out)
}

#[derive(Debug, Default)]
struct DepBlock {
    group: Option<String>,
    artifact: Option<String>,
    version: Option<String>,
    scope: Option<String>,
    optional: bool,
}

fn tag_value<'a>(block: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find(&close)? + start;
    Some(block[start..end].trim())
}

fn parse_dep_blocks(xml: &str) -> Vec<DepBlock> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<dependency>") {
        let Some(end) = rest[start..].find("</dependency>") else {
            break;
        };
        let block = &rest[start..start + end];
        out.push(DepBlock {
            group: tag_value(block, "groupId").map(str::to_string),
            artifact: tag_value(block, "artifactId").map(str::to_string),
            version: tag_value(block, "version").map(str::to_string),
            scope: tag_value(block, "scope").map(str::to_string),
            optional: tag_value(block, "optional") == Some("true"),
        });
        rest = &rest[start + end..];
    }
    out
}

fn parse_properties(pom: &str) -> std::collections::HashMap<String, String> {
    let mut props = std::collections::HashMap::new();
    if let (Some(a), Some(b)) = (pom.find("<properties>"), pom.find("</properties>"))
        && b > a
    {
        let body = &pom[a + "<properties>".len()..b];
        let mut rest = body;
        while let Some(open) = rest.find('<') {
            let Some(name_end) = rest[open + 1..].find('>') else {
                break;
            };
            let name = &rest[open + 1..open + 1 + name_end];
            if name.starts_with('/') || name.contains('!') {
                rest = &rest[open + 1..];
                continue;
            }
            let close = format!("</{name}>");
            let val_start = open + 1 + name_end + 1;
            let Some(val_end) = rest[val_start..].find(&close) else {
                rest = &rest[open + 1..];
                continue;
            };
            props.insert(
                name.to_string(),
                rest[val_start..val_start + val_end].trim().to_string(),
            );
            rest = &rest[val_start + val_end..];
        }
    }
    props
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pom_dependencies() {
        let pom = r#"
        <project>
          <properties><foo.version>2.0.0</foo.version></properties>
          <dependencyManagement><dependencies>
            <dependency><groupId>m</groupId><artifactId>pinned</artifactId><version>9.9</version></dependency>
          </dependencies></dependencyManagement>
          <dependencies>
            <dependency><groupId>a</groupId><artifactId>lib</artifactId><version>1.0</version></dependency>
            <dependency><groupId>b</groupId><artifactId>prop</artifactId><version>${foo.version}</version></dependency>
            <dependency><groupId>c</groupId><artifactId>test-only</artifactId><version>1</version><scope>test</scope></dependency>
            <dependency><groupId>d</groupId><artifactId>opt</artifactId><version>1</version><optional>true</optional></dependency>
            <dependency><groupId>m</groupId><artifactId>pinned</artifactId></dependency>
            <dependency><groupId>org.clojure</groupId><artifactId>clojure</artifactId><version>1.12.0</version></dependency>
          </dependencies>
        </project>"#;
        let owner = Coord {
            group: "o".into(),
            artifact: "owner".into(),
            version: "0.1".into(),
        };
        let deps = pom_dependencies(&owner, pom).unwrap();
        let names: Vec<String> = deps
            .iter()
            .map(|d| format!("{}:{}", d.ga(), d.version))
            .collect();
        // org.clojure/clojure is filtered later (EXCLUDED) — parsing keeps it.
        assert_eq!(
            names,
            vec![
                "a:lib:1.0",
                "b:prop:2.0.0",
                "m:pinned:9.9",
                "org.clojure:clojure:1.12.0"
            ]
        );
    }

    #[test]
    fn version_ranges_are_hard_errors() {
        let pom = r#"<dependencies><dependency>
            <groupId>a</groupId><artifactId>x</artifactId><version>[1.0,2.0)</version>
        </dependency></dependencies>"#;
        let owner = Coord {
            group: "o".into(),
            artifact: "o".into(),
            version: "1".into(),
        };
        assert!(pom_dependencies(&owner, pom).is_err());
    }
}
