use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use serde_json::{Value, json};

type Policy = BTreeMap<&'static str, &'static [&'static str]>;

#[derive(Clone, Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
}

#[derive(Clone, Deserialize)]
struct Package {
    id: String,
    name: String,
    manifest_path: PathBuf,
    dependencies: Vec<Dependency>,
}

#[derive(Clone, Deserialize)]
struct Dependency {
    name: String,
    path: Option<PathBuf>,
}

fn policy() -> Policy {
    BTreeMap::from([
        (
            "xtask",
            &[
                "ai-ex-inference",
                "ai-ex-model-catalog",
                "ai-ex-model-manager",
                "ai-ex-ollama",
            ][..],
        ),
        ("ai-ex-domain", &[][..]),
        ("ai-ex-protocol", &["ai-ex-domain"][..]),
        ("ai-ex-event-bus", &["ai-ex-domain", "ai-ex-protocol"][..]),
        ("ai-ex-stage", &["ai-ex-domain"][..]),
        ("ai-ex-stage-obs", &["ai-ex-domain", "ai-ex-stage"][..]),
        ("ai-ex-bilibili", &["ai-ex-domain", "ai-ex-event-bus"][..]),
        ("ai-ex-plugin", &["ai-ex-domain"][..]),
        (
            "ai-ex-simulator",
            &["ai-ex-domain", "ai-ex-event-bus", "ai-ex-memory"][..],
        ),
        ("ai-ex-config", &["ai-ex-domain"][..]),
        ("ai-ex-text", &["ai-ex-domain"][..]),
        (
            "ai-ex-core",
            &[
                "ai-ex-domain",
                "ai-ex-text",
                "ai-ex-protocol",
                "ai-ex-stage",
            ][..],
        ),
        ("ai-ex-duplex", &["ai-ex-domain"][..]),
        ("ai-ex-asr", &["ai-ex-domain", "ai-ex-duplex"][..]),
        ("ai-ex-capture", &["ai-ex-domain", "ai-ex-duplex"][..]),
        (
            "ai-ex-audio",
            &["ai-ex-core", "ai-ex-domain", "ai-ex-text", "ai-ex-stage"][..],
        ),
        ("ai-ex-memory", &["ai-ex-core", "ai-ex-domain"][..]),
        ("ai-ex-deepseek", &["ai-ex-core", "ai-ex-domain"][..]),
        (
            "ai-ex-ollama",
            &["ai-ex-core", "ai-ex-domain", "ai-ex-platform-windows"][..],
        ),
        ("ai-ex-platform-windows", &[][..]),
        ("ai-ex-koboldcpp", &["ai-ex-core", "ai-ex-domain"][..]),
        ("ai-ex-tts", &["ai-ex-domain"][..]),
        (
            "ai-ex-vts",
            &["ai-ex-core", "ai-ex-domain", "ai-ex-stage"][..],
        ),
        ("ai-ex-observability", &["ai-ex-core", "ai-ex-domain"][..]),
        ("ai-ex-safety", &["ai-ex-domain"][..]),
        ("ai-ex-automation", &["ai-ex-domain", "ai-ex-safety"][..]),
        (
            "ai-ex-audit",
            &["ai-ex-automation", "ai-ex-domain", "ai-ex-safety"][..],
        ),
        ("ai-ex-vision", &["ai-ex-domain"][..]),
        (
            "ai-ex-control",
            &["ai-ex-domain", "ai-ex-observability"][..],
        ),
        (
            "ai-ex-ui-model",
            &["ai-ex-domain", "ai-ex-observability"][..],
        ),
        (
            "ai-ex-migrate",
            &[
                "ai-ex-config",
                "ai-ex-domain",
                "ai-ex-memory",
                "ai-ex-platform-windows",
            ][..],
        ),
        ("ai-ex-model-catalog", &["ai-ex-domain"][..]),
        (
            "ai-ex-model-manager",
            &["ai-ex-domain", "ai-ex-model-catalog"][..],
        ),
        (
            "ai-ex-native-models",
            &[
                "ai-ex-core",
                "ai-ex-domain",
                "ai-ex-duplex",
                "ai-ex-config",
                "ai-ex-inference",
                "ai-ex-model-catalog",
                "ai-ex-model-manager",
            ][..],
        ),
        (
            "ai-ex-inference",
            &["ai-ex-domain", "ai-ex-model-catalog"][..],
        ),
        (
            "ai-ex-native-worker-support",
            &["ai-ex-domain", "ai-ex-inference", "ai-ex-model-catalog"][..],
        ),
        (
            "ai-ex-native-bge-worker",
            &["ai-ex-native-worker-support"][..],
        ),
        (
            "ai-ex-native-sherpa-worker",
            &["ai-ex-native-worker-support"][..],
        ),
        (
            "ai-ex-native-qwen-worker",
            &["ai-ex-native-worker-support"][..],
        ),
        (
            "ai-ex-desktop",
            &[
                "ai-ex-config",
                "ai-ex-control",
                "ai-ex-domain",
                "ai-ex-observability",
                "ai-ex-ui-model",
            ][..],
        ),
        (
            "ai-ex-service",
            &[
                "ai-ex-asr",
                "ai-ex-audit",
                "ai-ex-audio",
                "ai-ex-automation",
                "ai-ex-bilibili",
                "ai-ex-capture",
                "ai-ex-config",
                "ai-ex-control",
                "ai-ex-core",
                "ai-ex-domain",
                "ai-ex-event-bus",
                "ai-ex-deepseek",
                "ai-ex-duplex",
                "ai-ex-koboldcpp",
                "ai-ex-memory",
                "ai-ex-observability",
                "ai-ex-model-catalog",
                "ai-ex-model-manager",
                "ai-ex-native-models",
                "ai-ex-inference",
                "ai-ex-ollama",
                "ai-ex-plugin",
                "ai-ex-platform-windows",
                "ai-ex-safety",
                "ai-ex-stage",
                "ai-ex-stage-obs",
                "ai-ex-tts",
                "ai-ex-vision",
                "ai-ex-vts",
            ][..],
        ),
    ])
}

fn path_key(path: &Path) -> String {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => (),
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    let text = result.to_string_lossy().into_owned();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

fn violations(root: &Path, documents: &[Metadata], allowed: &Policy) -> Result<Vec<Value>, String> {
    let members: BTreeSet<_> = documents
        .iter()
        .flat_map(|d| d.workspace_members.iter())
        .collect();
    let mut packages: BTreeMap<&str, &Package> = BTreeMap::new();
    let mut manifests = BTreeSet::new();
    let mut errors = Vec::new();
    for package in documents
        .iter()
        .flat_map(|d| &d.packages)
        .filter(|p| members.contains(&p.id))
    {
        if packages.contains_key(package.name.as_str()) {
            errors.push(json!({"package": package.name, "dependency": "<duplicate package name>", "manifest": package.manifest_path}));
            continue;
        }
        packages.insert(&package.name, package);
        manifests.insert(path_key(&package.manifest_path));
        if !allowed.contains_key(package.name.as_str()) {
            errors.push(
                json!({"package": package.name, "dependency": "<missing architecture policy>"}),
            );
        }
    }
    for package in packages.values() {
        for dependency in &package.dependencies {
            let registered = packages.get(dependency.name.as_str());
            if let Some(path) = &dependency.path {
                let manifest = path.join("Cargo.toml");
                let reason = match registered {
                    None => Some("Local dependency is not a registered workspace package"),
                    Some(target) if path_key(&manifest) != path_key(&target.manifest_path) => {
                        Some("Local dependency does not use the registered manifest")
                    }
                    _ => None,
                };
                if let Some(reason) = reason {
                    errors.push(json!({"package": package.name, "dependency": dependency.name, "reason": reason, "manifest": manifest}));
                }
            } else if registered.is_some() {
                errors.push(json!({"package": package.name, "dependency": dependency.name, "reason": "Internal dependency must use the registered local path"}));
            }
            if registered.is_some()
                && !allowed
                    .get(package.name.as_str())
                    .is_some_and(|items| items.contains(&dependency.name.as_str()))
            {
                errors.push(json!({"package": package.name, "dependency": dependency.name}));
            }
        }
    }
    // Only immediate source roots count; build and fixture directories are excluded.
    for source in ["crates", "native-workers"] {
        let directory = root.join(source);
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("cannot read {}: {error}", directory.display())),
        };
        for entry in entries {
            let entry = entry
                .map_err(|error| format!("cannot inspect {}: {error}", directory.display()))?;
            if !entry.path().is_dir() {
                continue;
            }
            let manifest = entry.path().join("Cargo.toml");
            if manifest.is_file() && !manifests.contains(&path_key(&manifest)) {
                errors.push(json!({"package": entry.file_name().to_string_lossy(), "dependency": "<unregistered crate manifest>", "manifest": manifest}));
            }
        }
    }
    Ok(errors)
}

pub fn check(probe: bool) -> Result<(Value, bool), String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("missing workspace root")?;
    let manifests = [
        "Cargo.toml",
        "crates/ai-ex-desktop/Cargo.toml",
        "native-workers/Cargo.toml",
    ];
    let mut documents = Vec::new();
    for manifest in manifests {
        let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args([
                "metadata",
                "--no-deps",
                "--locked",
                "--offline",
                "--format-version",
                "1",
                "--manifest-path",
            ])
            .arg(root.join(manifest))
            .output()
            .map_err(|error| format!("cannot run cargo metadata: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "cargo metadata failed for {manifest}: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        documents.push(
            serde_json::from_slice::<Metadata>(&output.stdout)
                .map_err(|error| format!("invalid cargo metadata for {manifest}: {error}"))?,
        );
    }
    if probe {
        let service = documents
            .iter()
            .flat_map(|d| &d.packages)
            .find(|p| p.name == "ai-ex-service")
            .and_then(|p| p.manifest_path.parent())
            .ok_or("service package not found")?
            .to_path_buf();
        for package in documents.iter_mut().flat_map(|d| &mut d.packages) {
            if matches!(package.name.as_str(), "ai-ex-domain" | "ai-ex-desktop") {
                package.dependencies.push(Dependency {
                    name: "ai-ex-service".into(),
                    path: Some(service.clone()),
                });
            }
        }
    }
    let errors = violations(root, &documents, &policy())?;
    if !errors.is_empty() {
        return Ok((json!({"status": "failed", "violations": errors}), false));
    }
    let ids: BTreeSet<_> = documents
        .iter()
        .flat_map(|d| &d.workspace_members)
        .collect();
    Ok((
        json!({"status": "passed", "checked_packages": ids.len(), "workspace_packages": documents[0].workspace_members.len(), "desktop_packages": documents[1].workspace_members.len(), "native_worker_packages": documents[2].workspace_members.len()}),
        true,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

    fn fixture() -> (PathBuf, Vec<Metadata>, Policy) {
        let root = std::env::temp_dir().join(format!(
            "architecture-metadata-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let package = |name: &str| Package {
            id: name.into(),
            name: name.into(),
            manifest_path: root.join("crates").join(name).join("Cargo.toml"),
            dependencies: vec![],
        };
        let domain = package("domain");
        let mut adapter = package("adapter");
        adapter.dependencies.push(Dependency {
            name: "domain".into(),
            path: Some(root.join("crates/domain")),
        });
        let mut desktop = package("desktop");
        desktop.dependencies = adapter.dependencies.clone();
        let documents = vec![
            Metadata {
                workspace_members: vec![domain.id.clone(), adapter.id.clone()],
                packages: vec![domain, adapter],
            },
            Metadata {
                workspace_members: vec![desktop.id.clone()],
                packages: vec![desktop],
            },
        ];
        let policy = BTreeMap::from([
            ("domain", &[][..]),
            ("adapter", &["domain"][..]),
            ("desktop", &["domain"][..]),
        ]);
        (root, documents, policy)
    }

    #[test]
    fn accepts_multiple_workspaces_and_normalized_local_paths() {
        let (root, mut documents, policy) = fixture();
        documents[1].packages[0].dependencies[0].path = Some(root.join("crates/adapter/../domain"));
        assert!(violations(&root, &documents, &policy).unwrap().is_empty());
    }

    #[test]
    fn compares_local_paths_using_platform_case_rules() {
        let (root, mut documents, policy) = fixture();
        let domain = root.join("crates/domain");
        documents[1].packages[0].dependencies[0].path =
            Some(PathBuf::from(domain.to_string_lossy().to_uppercase()));
        let errors = violations(&root, &documents, &policy).unwrap();
        assert_eq!(errors.len(), if cfg!(windows) { 0 } else { 1 });
    }

    #[test]
    fn rejects_unregistered_and_wrong_or_registry_internal_dependencies() {
        let (root, documents, policy) = fixture();
        for (name, path, expected) in [
            (
                "private",
                Some(root.join("private")),
                "not a registered workspace package",
            ),
            (
                "domain",
                Some(root.join("crates/adapter")),
                "does not use the registered manifest",
            ),
            ("domain", None, "must use the registered local path"),
        ] {
            let mut changed = documents.clone();
            changed[1].packages[0].dependencies[0] = Dependency {
                name: name.into(),
                path,
            };
            let errors = violations(&root, &changed, &policy).unwrap();
            assert_eq!(errors.len(), 1);
            assert!(errors[0]["reason"].as_str().unwrap().contains(expected));
        }
    }

    #[test]
    fn checks_renamed_and_target_specific_dependencies_by_package_name() {
        let (root, mut documents, policy) = fixture();
        let valid_alias = json!({
            "name": "domain", "path": root.join("crates/domain"), "rename": "foundation"
        });
        documents[1].packages[0].dependencies = vec![serde_json::from_value(valid_alias).unwrap()];
        assert!(violations(&root, &documents, &policy).unwrap().is_empty());
        for kind in [Value::Null, json!("dev"), json!("build")] {
            let metadata = json!({
                "name": "adapter", "path": root.join("crates/adapter"),
                "rename": "local-alias", "kind": kind, "target": "cfg(windows)", "optional": true
            });
            documents[0].packages[0].dependencies = vec![serde_json::from_value(metadata).unwrap()];
            let errors = violations(&root, &documents, &policy).unwrap();
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0]["dependency"], "adapter");
        }
    }

    #[test]
    fn rejects_forbidden_edges_missing_policy_and_duplicate_packages() {
        let (root, mut documents, mut policy) = fixture();
        documents[0].packages[0].dependencies.push(Dependency {
            name: "adapter".into(),
            path: Some(root.join("crates/adapter")),
        });
        let errors = violations(&root, &documents, &policy).unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["dependency"], "adapter");
        policy.remove("domain");
        assert!(
            violations(&root, &documents, &policy)
                .unwrap()
                .iter()
                .any(|e| e["dependency"] == "<missing architecture policy>")
        );
        let duplicate = documents[0].packages[0].clone();
        documents[1].packages.push(duplicate);
        assert!(
            violations(&root, &documents, &policy)
                .unwrap()
                .iter()
                .any(|e| e["dependency"] == "<duplicate package name>")
        );
    }

    #[test]
    fn scans_only_immediate_crate_directories() {
        let (root, documents, policy) = fixture();
        let unregistered = root.join("native-workers/unregistered");
        let unregistered_crate = root.join("crates/unregistered");
        let nested = root.join("crates/adapter/target/generated");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir_all(&unregistered).unwrap();
        std::fs::create_dir_all(&unregistered_crate).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(unregistered.join("Cargo.toml"), "").unwrap();
        std::fs::write(unregistered_crate.join("Cargo.toml"), "").unwrap();
        std::fs::write(nested.join("Cargo.toml"), "").unwrap();
        let errors = violations(&root, &documents, &policy).unwrap();
        assert_eq!(errors.len(), 2);
        assert!(
            errors
                .iter()
                .all(|e| e["dependency"] == "<unregistered crate manifest>")
        );
        for file in [
            unregistered.join("Cargo.toml"),
            unregistered_crate.join("Cargo.toml"),
            nested.join("Cargo.toml"),
        ] {
            std::fs::remove_file(file).unwrap();
        }
        for directory in [
            unregistered,
            root.join("native-workers"),
            unregistered_crate,
            nested,
            root.join("crates/adapter/target"),
            root.join("crates/adapter"),
            root.join("crates"),
            root.clone(),
        ] {
            std::fs::remove_dir(directory).unwrap();
        }
    }
}
