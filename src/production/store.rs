use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

pub use crate::canonical::{digest, now, publish};
pub use crate::journal::Lease;

pub fn checked_path(root: &Path, relative_path: &str) -> Result<PathBuf, String> {
    relative(relative_path)?;
    let mut path = root.to_path_buf();
    for component in relative_path.split('/') {
        path.push(component);
        let metadata =
            fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
            return Err(format!(
                "symlink/special file unsupported: {}",
                path.display()
            ));
        }
        if metadata.is_dir() && path.join(".git").exists() {
            return Err(format!(
                "nested repository/submodule unsupported: {}",
                path.display()
            ));
        }
    }
    Ok(path)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    content: Option<String>,
    executable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tree {
    schema: String,
    entries: BTreeMap<String, Entry>,
}

pub struct Store(pub PathBuf);
impl Store {
    pub fn new(path: &Path) -> Result<Self, String> {
        Ok(Self(super::super::absolute(path)?))
    }
    /// Remove the working directories of attempts that have concluded.
    ///
    /// An attempt's `inputs`, `outputs` and `work` are swept when it finishes,
    /// but a run killed mid-attempt never reaches that point and leaves them
    /// behind for good. Sweeping when a command starts is the only place that
    /// debris can be recovered — and the only place that reclaims what older
    /// versions left.
    ///
    /// **A concluded attempt is one the journal records as `finished` or
    /// `abandoned`, and nothing else qualifies.** An unheld lease is not enough
    /// and was the first version of this: a lease proves no *executor*, not no
    /// *process*. An executor killed with SIGKILL never reaps the process group
    /// it started, so its shell and that shell's `cargo` or `make -j` children
    /// keep running and keep writing into `work` and `$SYKLI_OUTPUT` while the
    /// dead parent's lease is already released. Sweeping on the lease alone
    /// deletes those directories out from under a live build. The journal calls
    /// that state `contact-lost`, and it is exactly what must be left alone —
    /// both because something may still be writing, and because its scratch is
    /// the evidence an operator needs to decide whether to abandon it.
    ///
    /// The lease is still checked, as the second condition rather than the
    /// only one: a `finished` record plus a held lease means a new attempt has
    /// reused the directory. An error probing a lease counts as held.
    ///
    /// Best effort throughout. A sweep that cannot read or remove a directory
    /// must never stop the command it preceded.
    pub fn sweep_concluded(&self) -> usize {
        let Ok(requests) = fs::read_dir(self.0.join("requests")) else {
            return 0;
        };
        let mut swept = 0;
        for request in requests.flatten() {
            let request = request.path();
            let concluded = Self::concluded_attempts(&request.join("records"));
            if concluded.is_empty() {
                continue;
            }
            for name in concluded {
                let lease = request.join("attempt-leases").join(&name);
                if Lease::observe(&lease).unwrap_or(true) {
                    continue;
                }
                let attempt = request.join("attempts").join(&name);
                let mut removed = false;
                for scratch in ["inputs", "outputs", "work"] {
                    let path = attempt.join(scratch);
                    if path.exists() && fs::remove_dir_all(&path).is_ok() {
                        removed = true;
                    }
                }
                if removed {
                    swept += 1;
                }
            }
        }
        swept
    }

    /// Attempt ids this request's journal records as concluded.
    ///
    /// Read straight from the record files rather than through the typed
    /// history, which needs a parsed `Request` the sweep does not have and must
    /// not require: a store-wide sweep cannot depend on every request in the
    /// store still parsing. `Fact` is tagged `kind` in kebab-case, so
    /// `finished` and `abandoned` are the two terminal spellings. Anything
    /// unreadable is simply not concluded.
    fn concluded_attempts(records: &Path) -> Vec<String> {
        let Ok(entries) = fs::read_dir(records) else {
            return Vec::new();
        };
        let mut concluded = Vec::new();
        for entry in entries.flatten() {
            let Ok(bytes) = fs::read(entry.path()) else {
                continue;
            };
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                continue;
            };
            let fact = &value["record"]["fact"];
            if !matches!(fact["kind"].as_str(), Some("finished" | "abandoned")) {
                continue;
            }
            if let Some(attempt) = fact["attempt"].as_str() {
                concluded.push(attempt.to_string());
            }
        }
        concluded
    }

    pub fn production(&self, id: &str) -> Result<PathBuf, String> {
        digest(id)?;
        Ok(self.0.join("requests").join(id))
    }
    pub fn blob_path(&self, id: &str) -> Result<PathBuf, String> {
        digest(id)?;
        Ok(self.0.join("blobs").join(id))
    }
    pub fn blob(&self, bytes: &[u8]) -> Result<String, String> {
        let id = super::super::sha256(bytes);
        let path = self.blob_path(&id)?;
        publish(&path, bytes)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o555)).map_err(err)?;
        Ok(id)
    }
    pub fn read_blob(&self, id: &str) -> Result<Vec<u8>, String> {
        let path = self.blob_path(id)?;
        if !fs::symlink_metadata(&path).map_err(err)?.is_file() {
            return Err("artifact is not a regular file".into());
        }
        let bytes = fs::read(&path).map_err(|e| format!("artifact-unavailable {id}: {e}"))?;
        if super::super::sha256(&bytes) != id {
            return Err(format!("artifact digest mismatch {id}"));
        }
        Ok(bytes)
    }
    fn entry(&self, path: &Path) -> Result<Entry, String> {
        let metadata = fs::symlink_metadata(path).map_err(err)?;
        if !metadata.is_file() {
            return Err(format!("expected regular file: {}", path.display()));
        }
        // ponytail: whole-file collection; stream blobs if large artifacts justify it.
        Ok(Entry {
            content: Some(self.blob(&fs::read(path).map_err(err)?)?),
            executable: metadata.mode() & 0o111 != 0,
        })
    }
    pub fn capture(&self, root: &Path, source: &Source) -> Result<Artifact, String> {
        snapshot(root, source, |bytes| self.blob(bytes))
    }
    fn walk(
        &self,
        root: &Path,
        directory: &Path,
        entries: &mut BTreeMap<String, Entry>,
    ) -> Result<(), String> {
        for child in fs::read_dir(directory).map_err(err)? {
            let path = child.map_err(err)?.path();
            let relative_path = path
                .strip_prefix(root)
                .map_err(err)?
                .to_str()
                .ok_or("non-UTF8 artifact path")?;
            let checked = checked_path(root, relative_path)?;
            if checked.is_dir() {
                entries.insert(
                    relative_path.into(),
                    Entry {
                        content: None,
                        executable: false,
                    },
                );
                self.walk(root, &checked, entries)?;
            } else {
                entries.insert(relative_path.into(), self.entry(&checked)?);
            }
        }
        Ok(())
    }
    pub fn collect(&self, root: &Path, output: &Output) -> Result<Artifact, String> {
        let path = checked_path(root, &output.collect)?;
        let content = match &output.ty {
            ArtifactType::Directory => {
                if !path.is_dir() {
                    return Err("expected directory output".into());
                }
                let mut entries = BTreeMap::new();
                self.walk(&path, &path, &mut entries)?;
                self.blob(&canonical(&Tree {
                    schema: "sykli-tree.v1".into(),
                    entries,
                })?)?
            }
            ty => {
                let entry = self.entry(&path)?;
                let id = entry.content.unwrap();
                validate_bytes(ty, &self.read_blob(&id)?)?;
                if matches!(ty, ArtifactType::Executable { .. }) && !entry.executable {
                    return Err("executable output has no executable mode".into());
                }
                id
            }
        };
        Ok(Artifact {
            content,
            ty: output.ty.clone(),
        })
    }
    fn tree(&self, artifact: &Artifact) -> Result<Tree, String> {
        let tree: Tree = decode(&self.read_blob(&artifact.content)?)?;
        if tree.schema != "sykli-tree.v1" {
            return Err("unsupported tree schema".into());
        }
        for (path, entry) in &tree.entries {
            relative(path)?;
            for parent in Path::new(path).ancestors().skip(1) {
                if tree
                    .entries
                    .get(parent.to_str().ok_or("non-UTF8 tree")?)
                    .is_some_and(|e| e.content.is_some())
                {
                    return Err("tree file/directory collision".into());
                }
            }
            if let Some(id) = &entry.content {
                digest(id)?;
            } else if entry.executable {
                return Err("invalid directory entry".into());
            }
        }
        Ok(tree)
    }
    pub fn available(&self, artifact: &Artifact) -> Result<(), String> {
        match artifact.ty {
            ArtifactType::Directory | ArtifactType::SourceTree => {
                for entry in self.tree(artifact)?.entries.values() {
                    if let Some(id) = &entry.content {
                        self.read_blob(id)?;
                    }
                }
                Ok(())
            }
            _ => {
                validate_bytes(&artifact.ty, &self.read_blob(&artifact.content)?)?;
                if matches!(artifact.ty, ArtifactType::Executable { .. })
                    && fs::metadata(self.blob_path(&artifact.content)?)
                        .map_err(err)?
                        .mode()
                        & 0o111
                        == 0
                {
                    return Err(
                        "artifact-unavailable: executable location has no executable mode".into(),
                    );
                }
                Ok(())
            }
        }
    }
    pub fn materialize(&self, artifact: &Artifact, destination: &Path) -> Result<(), String> {
        match artifact.ty {
            ArtifactType::Directory | ArtifactType::SourceTree => {
                fs::create_dir(destination).map_err(err)?;
                for (p, entry) in self.tree(artifact)?.entries {
                    let path = destination.join(p);
                    if let Some(id) = entry.content {
                        fs::create_dir_all(path.parent().unwrap()).map_err(err)?;
                        fs::write(&path, self.read_blob(&id)?).map_err(err)?;
                        fs::set_permissions(
                            path,
                            fs::Permissions::from_mode(if entry.executable {
                                0o755
                            } else {
                                0o644
                            }),
                        )
                        .map_err(err)?;
                    } else {
                        fs::create_dir_all(path).map_err(err)?;
                    }
                }
            }
            _ => {
                let bytes = self.read_blob(&artifact.content)?;
                validate_bytes(&artifact.ty, &bytes)?;
                fs::write(destination, bytes).map_err(err)?;
                fs::set_permissions(
                    destination,
                    fs::Permissions::from_mode(
                        if matches!(artifact.ty, ArtifactType::Executable { .. }) {
                            0o755
                        } else {
                            0o644
                        },
                    ),
                )
                .map_err(err)?;
            }
        }
        Ok(())
    }
}

pub fn validate_bytes(ty: &ArtifactType, bytes: &[u8]) -> Result<(), String> {
    let ArtifactType::Executable {
        format,
        architecture,
    } = ty
    else {
        return Ok(());
    };
    let detected = if bytes.len() >= 64
        && &bytes[..4] == b"\x7fELF"
        && bytes[4] == 2
        && bytes[5] == 1
        && bytes[6] == 1
        && matches!(u16::from_le_bytes([bytes[16], bytes[17]]), 2 | 3)
    {
        (
            "elf",
            match u16::from_le_bytes([bytes[18], bytes[19]]) {
                62 => "x86_64",
                183 => "aarch64",
                _ => "unsupported",
            },
        )
    } else if bytes.len() >= 32
        && bytes[..4] == [0xcf, 0xfa, 0xed, 0xfe]
        && u32::from_le_bytes(bytes[12..16].try_into().unwrap()) == 2
    {
        (
            "macho",
            match u32::from_le_bytes(bytes[4..8].try_into().unwrap()) {
                0x01000007 => "x86_64",
                0x0100000c => "aarch64",
                _ => "unsupported",
            },
        )
    } else {
        return Err("unsupported or invalid executable header".into());
    };
    if detected != (format.as_str(), architecture.as_str()) {
        return Err(format!(
            "executable type mismatch: expected {format}/{architecture}, found {}/{}",
            detected.0, detected.1
        ));
    }
    Ok(())
}

pub fn snapshot(
    root: &Path,
    source: &Source,
    mut blob: impl FnMut(&[u8]) -> Result<String, String>,
) -> Result<Artifact, String> {
    let mut entries = BTreeMap::new();
    for path in &source.paths {
        let file = checked_path(root, path)?;
        let metadata = fs::symlink_metadata(&file).map_err(err)?;
        if !metadata.is_file() {
            return Err(format!("expected regular source file {path}"));
        }
        entries.insert(
            path.clone(),
            Entry {
                content: Some(blob(&fs::read(file).map_err(err)?)?),
                executable: metadata.mode() & 0o111 != 0,
            },
        );
    }
    Ok(Artifact {
        content: blob(&canonical(&Tree {
            schema: "sykli-tree.v1".into(),
            entries,
        })?)?,
        ty: source.ty.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a concluded attempt is swept, and only when nobody holds it.
    ///
    /// Three attempts, one of each shape that matters:
    ///
    /// * `finished` and unheld — the ordinary case, swept;
    /// * `finished` but held — a new attempt has reused the directory, spared;
    /// * `contact-lost` and unheld — the dangerous one. Its executor was killed
    ///   without reaping the process group it started, so its shell's children
    ///   may still be writing into `work` and `$SYKLI_OUTPUT` even though the
    ///   dead parent's lease is long released. Sweeping on the lease alone
    ///   deletes those directories out from under a live build, and destroys
    ///   the scratch an operator needs before deciding to abandon it.
    #[test]
    fn only_a_concluded_attempt_is_swept_and_only_when_unheld() {
        let root = std::env::temp_dir().join(format!(
            "sykli-sweep-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let request = root.join("requests").join("r".repeat(64));
        let done = "a".repeat(64);
        let held_done = "b".repeat(64);
        let lost = "c".repeat(64);
        for attempt in [&done, &held_done, &lost] {
            for scratch in ["inputs", "outputs", "work"] {
                let path = request.join("attempts").join(attempt).join(scratch);
                fs::create_dir_all(&path).unwrap();
                fs::write(path.join("junk"), b"x").unwrap();
            }
        }

        let records = request.join("records");
        fs::create_dir_all(&records).unwrap();
        for (index, (attempt, kind)) in [
            (&done, "finished"),
            (&held_done, "finished"),
            (&lost, "contact-lost"),
        ]
        .into_iter()
        .enumerate()
        {
            fs::write(
                records.join(format!("{:020}.json", index + 1)),
                serde_json::to_vec(&serde_json::json!({
                    "record": {"fact": {"kind": kind, "attempt": attempt}}
                }))
                .unwrap(),
            )
            .unwrap();
        }

        // Hold one finished attempt exactly as a reusing executor would.
        let held = Lease::attempt(&request, &held_done).unwrap();

        let store = Store::new(&root).unwrap();
        assert_eq!(store.sweep_concluded(), 1, "only the finished, unheld one");

        for scratch in ["inputs", "outputs", "work"] {
            assert!(
                !request.join("attempts").join(&done).join(scratch).exists(),
                "a concluded attempt's {scratch} must go"
            );
            assert!(
                request
                    .join("attempts")
                    .join(&held_done)
                    .join(scratch)
                    .exists(),
                "a held attempt must keep its {scratch}"
            );
            assert!(
                request.join("attempts").join(&lost).join(scratch).exists(),
                "a contact-lost attempt must keep its {scratch}: its descendants \
                 may still be writing, and it is the evidence for abandoning it"
            );
        }

        // Releasing the lease makes the finished one reclaimable. The
        // contact-lost one stays, because no lease ever protected it — only its
        // record does.
        drop(held);
        assert_eq!(store.sweep_concluded(), 1);
        assert!(
            !request
                .join("attempts")
                .join(&held_done)
                .join("outputs")
                .exists()
        );
        assert!(
            request
                .join("attempts")
                .join(&lost)
                .join("outputs")
                .exists(),
            "contact-lost is never swept, whatever the lease says"
        );

        fs::remove_dir_all(&root).unwrap();
    }
}
