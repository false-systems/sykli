//! Ask build tools what exists; emit ordinary production operations.
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

pub struct GoTarget {
    pub package: String,
    pub binary: String,
    pub paths: Vec<String>,
    pub build: String,
    pub test: String,
}

pub fn go(package: Option<&str>) -> Result<GoTarget, String> {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        _ => return Err("unsupported Go executable host".into()),
    };
    let architecture = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        _ => return Err("Go discovery supports arm64 and amd64 hosts".into()),
    };
    let environment = [
        ("GOENV", "off"),
        ("GOWORK", "off"),
        ("GOTOOLCHAIN", "local"),
        ("GOPROXY", "off"),
        ("GOSUMDB", "off"),
        ("CGO_ENABLED", "0"),
        ("GOOS", os),
        ("GOARCH", architecture),
    ];
    let query = |args: &[&str]| -> Result<String, String> {
        let runtime = super::super::shell_runtime()?;
        let result = ProcessCommand::new("go")
            .args(args)
            .env_clear()
            .envs(runtime.environment)
            .envs(environment)
            .output()
            .map_err(err)?;
        if !result.status.success() {
            return Err(format!(
                "Go discovery (offline): {}",
                String::from_utf8_lossy(&result.stderr).trim()
            ));
        }
        String::from_utf8(result.stdout).map_err(err)
    };
    let root = std::env::current_dir()
        .map_err(err)?
        .canonicalize()
        .map_err(err)?;
    checked_path(&root, "go.mod")?;
    let module: Value = serde_json::from_str(&query(&["mod", "edit", "-json"])?).map_err(err)?;
    if module["Replace"].as_array().is_some_and(|r| !r.is_empty()) || root.join("vendor").exists() {
        return Err("Go replacements and vendoring require an explicit production contract".into());
    }
    let listing = query(&["list", "-mod=readonly", "-json", "./..."])?;
    let mut paths = BTreeSet::from(["go.mod".to_string()]);
    if root.join("go.sum").exists() {
        paths.insert("go.sum".into());
    }
    let mut candidates = Vec::new();
    for item in serde_json::Deserializer::from_str(&listing).into_iter::<Value>() {
        let item = item.map_err(err)?;
        let directory = Path::new(item["Dir"].as_str().ok_or("Go package missing Dir")?);
        let prefix = directory.strip_prefix(&root).map_err(err)?;
        let relative_package = if prefix.as_os_str().is_empty() {
            ".".into()
        } else {
            format!("./{}", prefix.to_str().ok_or("non-UTF8 Go path")?)
        };
        if item["Name"] == "main"
            && package.is_none_or(|p| p == relative_package || item["ImportPath"] == p)
        {
            let binary = Path::new(
                item["Target"]
                    .as_str()
                    .ok_or("Go main package missing Target")?,
            )
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Go main package has no executable name")?;
            candidates.push((relative_package, binary.to_owned()));
        }
        for field in [
            "GoFiles",
            "TestGoFiles",
            "XTestGoFiles",
            "SFiles",
            "HFiles",
            "SysoFiles",
            "EmbedFiles",
            "TestEmbedFiles",
            "XTestEmbedFiles",
        ] {
            if let Some(files) = item[field].as_array() {
                for file in files {
                    let path = prefix.join(file.as_str().ok_or("invalid Go file name")?);
                    paths.insert(path.to_str().ok_or("non-UTF8 Go source")?.to_string());
                }
            }
        }
        // Test fixtures are not listed by go list, but go test runs beside them.
        let testdata = prefix.join("testdata");
        if root.join(&testdata).exists() {
            go_testdata(&root, &testdata, &mut paths)?;
        }
    }
    if candidates.len() != 1 {
        return Err(format!(
            "expected one Go main package; found [{}]. Select --package ./cmd/NAME (library-only modules need an explicit contract)",
            candidates
                .iter()
                .map(|(package, _)| package.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for path in &paths {
        source_path(path)?;
        if !checked_path(&root, path)?.is_file() {
            return Err(format!("Go source must be a regular file: {path}"));
        }
    }
    let (package, binary) = candidates.remove(0);
    let environment = environment
        .iter()
        .map(|(k, v)| format!("{k}={}", quote(v)))
        .collect::<Vec<_>>()
        .join(" ");
    let command = format!(
        "cd \"$SYKLI_INPUT_source\" && {environment} GOCACHE=\"$SYKLI_OUTPUT/go-cache\" go"
    );
    Ok(GoTarget {
        build: format!(
            "{command} build -mod=readonly -buildvcs=false -o \"$SYKLI_OUTPUT/app\" {}",
            quote(&package)
        ),
        test: format!("{command} test -mod=readonly -buildvcs=false -count=1 ./..."),
        package,
        binary,
        paths: paths.into_iter().collect(),
    })
}

fn go_testdata(root: &Path, relative: &Path, paths: &mut BTreeSet<String>) -> Result<(), String> {
    let name = relative.to_str().ok_or("non-UTF8 Go testdata")?;
    source_path(name)?;
    let path = checked_path(root, name)?;
    if path.is_dir() {
        for entry in fs::read_dir(path).map_err(err)? {
            go_testdata(root, &relative.join(entry.map_err(err)?.file_name()), paths)?;
        }
    } else {
        paths.insert(name.into());
    }
    Ok(())
}
