//! Immutable evidence bundles: content-addressed raw responses under one
//! manifest, published atomically and loaded with reference checks. Private
//! diagnostics sit beside the manifest and are opened only on request.
//!
//! The bundle trusts the local collector and its storage. Digests detect
//! accidental change and torn publication, not a replaced bundle.
use crate::assessment::Gap;
use crate::canonical::{NONCE, canonical, decode, digest, identity, now, publish};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

pub const COLLECTION_SCHEMA: &str = "sykli-collection.v1";

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// One retained provider response. `object` is the SHA-256 of the body bytes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub role: String,
    pub page: u64,
    pub endpoint: String,
    pub status: u16,
    pub object: String,
    pub bytes: u64,
    pub requested_at: String,
    pub request_id: Option<String>,
    pub next: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub start: String,
    pub end: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: String,
    pub reader: String,
    pub host: String,
    pub repository: String,
    pub pull_request: u64,
    pub interval: Window,
    pub selectors: BTreeMap<String, String>,
    /// Why each listing stopped: exhausted, page-cap, http STATUS, transport, not-json.
    pub terminations: BTreeMap<String, String>,
    pub responses: Vec<Response>,
    pub gaps: Vec<Gap>,
    pub requirements: Option<String>,
    pub previous_collection: Option<String>,
    pub tool: String,
}

impl Manifest {
    pub fn id(&self) -> Result<String, String> {
        if self.schema != COLLECTION_SCHEMA {
            return Err(format!("expected schema {COLLECTION_SCHEMA}"));
        }
        identity(COLLECTION_SCHEMA, self)
    }
}

pub struct Store {
    root: PathBuf,
}

/// A loaded, reference-checked collection.
pub struct Bundle {
    pub id: String,
    pub path: PathBuf,
    pub manifest: Manifest,
    objects: BTreeMap<String, Vec<u8>>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        Ok(Self {
            root: crate::absolute(path)?,
        })
    }

    /// Publish a new collection: objects and manifest land in a private
    /// temporary directory, then one rename makes the whole bundle visible.
    pub fn publish(
        &self,
        manifest: &Manifest,
        objects: &BTreeMap<String, Vec<u8>>,
        diagnostics: &Value,
    ) -> Result<Bundle, String> {
        for response in &manifest.responses {
            let bytes = objects
                .get(&response.object)
                .ok_or_else(|| format!("manifest references missing object {}", response.object))?;
            if crate::sha256(bytes) != response.object || bytes.len() as u64 != response.bytes {
                return Err(format!(
                    "object {} does not match its digest",
                    response.object
                ));
            }
        }
        let id = manifest.id()?;
        fs::create_dir_all(&self.root).map_err(err)?;
        let temporary = self.root.join(format!(
            ".tmp-{}-{}-{}",
            std::process::id(),
            now(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> Result<(), String> {
            fs::create_dir(&temporary).map_err(err)?;
            for (object, bytes) in objects {
                digest(object)?;
                publish(&temporary.join("objects").join(object), bytes)?;
            }
            publish(&temporary.join("manifest.json"), &canonical(manifest)?)?;
            publish(
                &temporary.join("diagnostics.json"),
                &serde_json::to_vec_pretty(diagnostics).map_err(err)?,
            )?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_dir_all(&temporary);
            return Err(error);
        }
        let path = self.root.join(&id);
        match fs::rename(&temporary, &path) {
            Ok(()) => {}
            Err(_) if path.join("manifest.json").is_file() => {
                let existing = fs::read(path.join("manifest.json")).map_err(err)?;
                let _ = fs::remove_dir_all(&temporary);
                if existing != canonical(manifest)? {
                    return Err(format!(
                        "conflicting collection already published at {}",
                        path.display()
                    ));
                }
            }
            Err(error) => {
                let _ = fs::remove_dir_all(&temporary);
                return Err(err(error));
            }
        }
        Bundle::load(&path)
    }

    /// The most recent published collection for the same pull request, if any.
    /// Only digest-named directories whose manifest matches their name count;
    /// temporary directories left by an interrupted publish are ignored. Reads
    /// manifests only, never objects.
    pub fn latest(&self, repository: &str, pull_request: u64) -> Option<String> {
        let mut best: Option<(String, String)> = None;
        for entry in fs::read_dir(&self.root).ok()?.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if digest(name).is_err() {
                continue;
            }
            let Ok(bytes) = fs::read(entry.path().join("manifest.json")) else {
                continue;
            };
            let Ok(manifest) = decode::<Manifest>(&bytes) else {
                continue;
            };
            if manifest.id().ok().as_deref() != Some(name)
                || !manifest.repository.eq_ignore_ascii_case(repository)
                || manifest.pull_request != pull_request
            {
                continue;
            }
            let key = (manifest.interval.end.clone(), name.to_string());
            if best.as_ref().is_none_or(|b| *b < key) {
                best = Some(key);
            }
        }
        best.map(|(_, id)| id)
    }
}

impl Bundle {
    pub fn load(path: &Path) -> Result<Self, String> {
        let path = crate::absolute(path)?;
        let manifest_path = path.join("manifest.json");
        let bytes = fs::read(&manifest_path)
            .map_err(|e| format!("not an evidence bundle: {}: {e}", manifest_path.display()))?;
        let manifest: Manifest = decode(&bytes).map_err(|e| format!("invalid manifest: {e}"))?;
        let id = manifest.id()?;
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if digest(name).is_ok() && name != id {
            return Err(format!(
                "bundle directory {name} does not match its manifest identity {id}"
            ));
        }
        let mut objects = BTreeMap::new();
        for response in &manifest.responses {
            digest(&response.object)?;
            if objects.contains_key(&response.object) {
                continue;
            }
            let object_path = path.join("objects").join(&response.object);
            let bytes = fs::read(&object_path)
                .map_err(|e| format!("missing object {}: {e}", response.object))?;
            if crate::sha256(&bytes) != response.object || bytes.len() as u64 != response.bytes {
                return Err(format!(
                    "object {} does not match its digest",
                    response.object
                ));
            }
            objects.insert(response.object.clone(), bytes);
        }
        Ok(Self {
            id,
            path,
            manifest,
            objects,
        })
    }

    pub fn object(&self, id: &str) -> Result<&[u8], String> {
        self.objects
            .get(id)
            .map(Vec::as_slice)
            .ok_or_else(|| format!("unreferenced object {id}"))
    }

    /// Save an immutable derived record (requirements, request, assessment)
    /// beside the collection. Identical content is accepted; conflict is an error.
    pub fn save(&self, kind: &str, id: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        digest(id)?;
        let path = self.path.join(kind).join(format!("{id}.json"));
        publish(&path, bytes)?;
        Ok(path)
    }
}
