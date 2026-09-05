//! Ask Cargo what exists; emit ordinary production operations, not a second build model.
use super::*;

pub struct CargoTarget {
    pub package: String,
    pub binary: String,
    pub paths: Vec<String>,
    pub build: String,
    pub test: String,
}

fn output(program: &str, args: &[&str]) -> Result<String, String> {
    let result = ProcessCommand::new(program)
        .args(args)
        .output()
        .map_err(err)?;
    if !result.status.success() {
        return Err(format!(
            "{program} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    String::from_utf8(result.stdout).map_err(err)
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("Cargo metadata missing {key}"))
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn cargo(
    authoring_path: &Path,
    package: Option<&str>,
    binary: Option<&str>,
) -> Result<CargoTarget, String> {
    let metadata: Value = serde_json::from_str(&output(
        "cargo",
        &[
            "metadata",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
        ],
    )?)
    .map_err(err)?;
    let root = std::env::current_dir()
        .map_err(err)?
        .canonicalize()
        .map_err(err)?;
    if Path::new(string(&metadata, "workspace_root")?)
        .canonicalize()
        .map_err(err)?
        != root
    {
        return Err("run typed Cargo init from the workspace root".into());
    }
    let packages = metadata["packages"]
        .as_array()
        .ok_or("Cargo metadata missing packages")?;
    let defaults = metadata["workspace_default_members"]
        .as_array()
        .ok_or("Cargo metadata missing default members")?;
    let mut candidates = Vec::new();
    for p in packages {
        if package.is_some_and(|name| p["name"] != name)
            || (package.is_none() && binary.is_none() && !defaults.contains(&p["id"]))
        {
            continue;
        }
        for target in p["targets"]
            .as_array()
            .ok_or("Cargo metadata missing targets")?
        {
            if target["kind"]
                .as_array()
                .is_some_and(|k| k.iter().any(|k| k == "bin"))
                && binary.is_none_or(|b| target["name"] == b)
            {
                candidates.push((p, target));
            }
        }
    }
    if candidates.len() != 1 {
        let choices: Vec<_> = candidates
            .iter()
            .map(|(p, t)| {
                format!(
                    "{}:{}",
                    p["name"].as_str().unwrap_or("?"),
                    t["name"].as_str().unwrap_or("?")
                )
            })
            .collect();
        return Err(format!(
            "expected one Cargo binary; found [{}]. Select --package NAME --bin NAME",
            choices.join(", ")
        ));
    }
    let (selected, target) = candidates[0];
    if target["required-features"]
        .as_array()
        .is_some_and(|f| !f.is_empty())
    {
        return Err("feature-gated binaries need an explicit production contract; automatic feature selection is unsupported".into());
    }
    let package = string(selected, "name")?.to_owned();
    let binary = string(target, "name")?.to_owned();
    let mut manifests = BTreeSet::new();
    for p in packages {
        let manifest = Path::new(string(p, "manifest_path")?)
            .canonicalize()
            .map_err(err)?;
        if !manifest.starts_with(&root) {
            return Err("workspace members outside the workspace root are unsupported".into());
        }
        manifests.insert(manifest);
    }
    for p in packages {
        for dep in p["dependencies"]
            .as_array()
            .ok_or("Cargo metadata missing dependencies")?
        {
            if let Some(path) = dep["path"].as_str() {
                let manifest = Path::new(path)
                    .join("Cargo.toml")
                    .canonicalize()
                    .map_err(err)?;
                if !manifests.contains(&manifest) {
                    return Err(format!(
                        "path dependency {path} must be a workspace member for automatic discovery"
                    ));
                }
            }
        }
    }
    if !root.join("Cargo.lock").is_file() {
        output("cargo", &["generate-lockfile", "--offline"])?;
    }
    let mut paths = BTreeSet::from(["Cargo.toml".to_string(), "Cargo.lock".to_string()]);
    for p in packages {
        let manifest = Path::new(string(p, "manifest_path")?)
            .canonicalize()
            .map_err(err)?;
        let prefix = manifest
            .parent()
            .unwrap()
            .strip_prefix(&root)
            .map_err(err)?;
        let list = output(
            "cargo",
            &[
                "package",
                "--list",
                "--allow-dirty",
                "--offline",
                "--manifest-path",
                manifest.to_str().ok_or("non-UTF8 Cargo manifest")?,
            ],
        )?;
        for file in list.lines() {
            if matches!(file, "Cargo.toml.orig" | ".cargo_vcs_info.json") {
                continue;
            }
            relative(file)?;
            let path = prefix.join(file);
            let path = path.to_str().ok_or("non-UTF8 Cargo source path")?;
            if Path::new(path) == authoring_path || source_path(path).is_err() {
                continue;
            }
            // Cargo lists a generated lockfile for members even when only the workspace lock exists.
            if file == "Cargo.lock" && !root.join(path).exists() {
                continue;
            }
            let source = checked_path(&root, path)?;
            if !source.is_file() {
                return Err(format!("Cargo source must be a regular file: {path}"));
            }
            paths.insert(path.into());
        }
        for target in p["targets"]
            .as_array()
            .ok_or("Cargo metadata missing targets")?
        {
            let source = Path::new(string(target, "src_path")?)
                .strip_prefix(&root)
                .map_err(err)?;
            if !paths.contains(source.to_str().ok_or("non-UTF8 target path")?) {
                return Err(format!(
                    "Cargo target source {} is excluded; declare an explicit contract",
                    source.display()
                ));
            }
        }
    }
    // Cargo can exclude configuration from its package; execution still needs the reviewed file.
    for config in [".cargo/config", ".cargo/config.toml"] {
        if root.join(config).exists() {
            checked_path(&root, config)?;
            paths.insert(config.into());
        }
    }
    let verbose = output("rustc", &["-vV"])?;
    let host = verbose
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or("rustc did not identify its host target")?;
    let arguments = format!(
        "--offline --locked --package {} --target {} --target-dir \"$SYKLI_OUTPUT/target\"",
        quote(&package),
        quote(host)
    );
    let build = format!(
        "cd \"$SYKLI_INPUT_source\" && cargo build {arguments} --release --bin {} && cp \"$SYKLI_OUTPUT/target\"/{} \"$SYKLI_OUTPUT/app\"",
        quote(&binary),
        quote(&format!("{host}/release/{binary}"))
    );
    let library = selected["targets"].as_array().unwrap().iter().any(|t| {
        t["kind"].as_array().is_some_and(|k| {
            k.iter().any(|k| {
                matches!(
                    k.as_str(),
                    Some("lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro")
                )
            })
        })
    });
    let test = format!(
        "cd \"$SYKLI_INPUT_source\" && cargo test {arguments} --bins{}",
        if library { " --lib" } else { "" }
    );
    Ok(CargoTarget {
        package,
        binary,
        paths: paths.into_iter().collect(),
        build,
        test,
    })
}
