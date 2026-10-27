//! What a Cargo build reads, as `init` declares it and the completeness
//! rule requires it (`validate`, `run`, `plan --explain`), and which files
//! are documents, which the undeclared-sibling check never asks for. One
//! definition of each, so the contract `init` writes is one the rules
//! accept, and stays accepted when documents are added.
//!
//! A Cargo build reads Cargo's own files (`by_name`), the files its Rust
//! sources include (`Sources`), every file of a package `cargo vendor` wrote,
//! and what its build scripts' string literals name (`Reads`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Whether `relative`, a path from the directory the contract's paths start
/// at, is a file Cargo or its tools read by name: Rust source, a manifest or
/// lock, toolchain, rustfmt or clippy configuration, Cargo's configuration,
/// or anything under a `tests` directory.
pub(crate) fn by_name(relative: &Path) -> bool {
    let name = relative
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    relative.extension().is_some_and(|ext| ext == "rs")
        || matches!(
            name,
            "Cargo.toml"
                | "Cargo.lock"
                | "rust-toolchain"
                | "rust-toolchain.toml"
                | "rustfmt.toml"
                | ".rustfmt.toml"
                | "clippy.toml"
                | ".clippy.toml"
        )
        || (relative
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == ".cargo")
            && matches!(name, "config" | "config.toml"))
        || relative
            .components()
            .any(|part| part.as_os_str() == "tests")
}

/// Whether `relative` is documentation, a licence or CI configuration, at
/// any depth: a file named `*.md`, or whose name starts with `LICENSE`,
/// `LICENCE`, `COPYING` or `NOTICE` (in any case), or a file under a
/// directory named `docs` or `.github`. The undeclared-sibling check does
/// not ask for these, so a commit that adds documents never makes a contract
/// refuse. `init` declares them like any other file (a test may read a
/// folder of Markdown cases); the Cargo rule requires one that a Rust source
/// includes, or that Cargo reads by name (`by_name`), and warns of one a
/// string in a source names (`Sources::named_at`).
pub(crate) fn docs(relative: &Path) -> bool {
    let names = relative
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>();
    let Some((file, directories)) = names.split_last() else {
        return false;
    };
    let upper = file.to_ascii_uppercase();
    upper.ends_with(".MD")
        || ["LICENSE", "LICENCE", "COPYING", "NOTICE"]
            .iter()
            .any(|prefix| upper.starts_with(prefix))
        || directories
            .iter()
            .any(|directory| matches!(*directory, "docs" | ".github"))
}

/// What a Cargo build reads among the files git lists, beyond Cargo's files
/// by name (`by_name`), which `init` declares and the completeness rule
/// requires alike:
/// - what its Rust sources include (`Sources`);
/// - every file of a package with a `.cargo-checksum.json` beside its
///   `Cargo.toml`, as `cargo vendor` writes one: where Cargo verifies that
///   checksum, it hashes every file the checksum lists on every build and
///   refuses to build when one has changed, so the build reads each of
///   them, and anywhere else requiring them all costs only cache misses;
/// - what a build script's string literals name, from its package's
///   directory (`build_named`): a build script runs during the build there,
///   and hands each path it names to something that reads it (`std::fs`, a
///   compiler, a code generator), or to Cargo (`cargo:rerun-if-changed=`,
///   resolved from the same directory). A file, or every file under a
///   directory. What it reads by a path built at run time is not seen
///   (`build_scripts`, which `validate` and `run` warn of).
///
/// An include whose path is not a literal (a third-party test macro's
/// `include_bytes!($file)`) is not refused only in a package the tree
/// proves Cargo verifies, and so treats as a published package that cannot
/// read outside itself (`vendored`); its build script is not warned of
/// either. A checksum file alone proves nothing: Cargo checks it only for a
/// package in a replaced source's directory, so one beside a first-party
/// manifest, planted or copied with a package out of that directory, leaves
/// the package refused and warned of as any other.
pub(crate) struct Reads {
    sources: Sources,
    /// The directories of the packages with a checksum file, read whole.
    checksummed: BTreeSet<PathBuf>,
    /// The build scripts outside vendored packages.
    scripts: Vec<PathBuf>,
    /// The paths build scripts' string literals name: files, or directories.
    named: BTreeSet<PathBuf>,
}

impl Reads {
    /// What the build reads among `files`, every file git lists (tracked,
    /// or untracked and not ignored), spelled as the caller spells paths
    /// (from the current directory, or absolute); `root`, the repository's
    /// top level spelled the same way, is where shown paths start. Every
    /// Rust source among them is read.
    pub(crate) fn of(root: &Path, files: &[PathBuf]) -> Result<Reads, String> {
        let listed = files.iter().map(PathBuf::as_path).collect::<BTreeSet<_>>();
        let named_file =
            |path: &Path, name: &str| path.file_name().is_some_and(|file| file == name);
        let checksummed = files
            .iter()
            .filter(|path| named_file(path, ".cargo-checksum.json"))
            .filter_map(|path| path.parent())
            .filter(|directory| listed.contains(directory.join("Cargo.toml").as_path()))
            .map(Path::to_path_buf)
            .collect::<BTreeSet<_>>();
        let vendored = vendored(files, &checksummed);
        // Each build script, with its package's directory.
        let mut scripts = BTreeMap::new();
        for manifest in files.iter().filter(|path| named_file(path, "Cargo.toml")) {
            let directory = manifest.parent().unwrap_or_else(|| Path::new(""));
            let script = match build_setting(&fs::read_to_string(manifest).unwrap_or_default()) {
                Build::Default => Some(directory.join("build.rs")),
                Build::Path(path) => lexical(&directory.join(path)),
                Build::Off => None,
            };
            if let Some(script) = script.filter(|script| listed.contains(script.as_path())) {
                scripts.insert(script, directory.to_path_buf());
            }
        }
        let mut sources = Sources {
            vendored: vendored.clone(),
            ..Sources::default()
        };
        let mut named = BTreeSet::new();
        for path in files {
            if !(path.extension().is_some_and(|ext| ext == "rs") && path.is_file()) {
                continue;
            }
            let text = fs::read(path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            let text = String::from_utf8_lossy(&text);
            let shown = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            sources.read(path, &text, &shown);
            if let Some(package) = scripts.get(path) {
                build_named(&text, package, &mut named);
            }
        }
        let scripts = scripts
            .into_keys()
            .filter(|script| !under(&vendored, script))
            .collect();
        Ok(Reads {
            sources,
            checksummed,
            scripts,
            named,
        })
    }

    /// Whether the build reads `path`, one of the files `of` was given, by
    /// what it holds rather than by its name: a source includes it, it is
    /// in a package with a checksum file, or a build script names it or a
    /// directory above it.
    pub(crate) fn reads(&self, path: &Path) -> bool {
        self.sources.included.contains(path)
            || under(&self.checksummed, path)
            || under(&self.named, path)
    }

    /// The build scripts outside vendored packages: what they read by a
    /// path built at run time is not seen.
    pub(crate) fn build_scripts(&self) -> &[PathBuf] {
        &self.scripts
    }

    pub(crate) fn sources(&self) -> &Sources {
        &self.sources
    }
}

/// Of the packages with a checksum file (`checksummed`), those the tree
/// proves Cargo verifies, as it does every package of a replaced source:
/// - the package's directory is directly inside a directory that a tracked
///   Cargo configuration (`.cargo/config.toml` or `.cargo/config`) names as
///   a replaced source's (`[source.NAME] directory = "…"`), resolved from
///   the directory its `.cargo` is in, as Cargo resolves it; `cargo vendor`
///   says to write exactly that;
/// - a tracked `Cargo.lock` pins a package of the manifest's name and
///   version from a registry or git (`source = …`), so Cargo takes it from
///   that source, replaced by the directory, and not as a path package (a
///   `[patch]` of it, say, which Cargo never verifies);
/// - where the lock records a checksum, the checksum file records the same
///   one for the package, as Cargo itself requires.
///
/// Whether every file still matches its listed hash is left to Cargo, which
/// refuses to build otherwise: it bears on no file outside the package.
fn vendored(files: &[PathBuf], checksummed: &BTreeSet<PathBuf>) -> BTreeSet<PathBuf> {
    if checksummed.is_empty() {
        return BTreeSet::new();
    }
    let read = |path: &Path| fs::read_to_string(path).unwrap_or_default();
    let mut directories = BTreeSet::new();
    let mut pins = BTreeMap::new();
    for path in files {
        let name = path.file_name().and_then(|name| name.to_str());
        let configuration = matches!(name, Some("config.toml" | "config"))
            && path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|parent| parent == ".cargo");
        if configuration {
            let base = path
                .parent()
                .and_then(Path::parent)
                .unwrap_or_else(|| Path::new(""));
            for entry in entries(&read(path)) {
                let directory = entry.quoted
                    && ((entry.table.starts_with("source.") && entry.key == "directory")
                        || (entry.table == "source" && entry.key.ends_with(".directory"))
                        || (entry.table.is_empty()
                            && entry.key.starts_with("source.")
                            && entry.key.ends_with(".directory")));
                if directory {
                    directories.extend(lexical(&base.join(&entry.value)));
                }
            }
        }
        if name == Some("Cargo.lock") {
            pins.extend(pinned(&read(path)));
        }
    }
    checksummed
        .iter()
        .filter(|package| {
            package
                .parent()
                .is_some_and(|parent| directories.contains(parent))
        })
        .filter(|package| {
            let text = read(&package.join("Cargo.toml"));
            let manifest = entries(&text);
            let field = |key: &str| {
                manifest
                    .iter()
                    .find(|entry| entry.table == "package" && entry.key == key && entry.quoted)
                    .map(|entry| entry.value.clone())
            };
            let (Some(name), Some(version)) = (field("name"), field("version")) else {
                return false;
            };
            let Some(pinned) = pins.get(&(name, version)) else {
                return false;
            };
            let recorded = serde_json::from_str::<serde_json::Value>(&read(
                &package.join(".cargo-checksum.json"),
            ))
            .ok()
            .and_then(|checksum| checksum["package"].as_str().map(str::to_string));
            pinned.is_none() || *pinned == recorded
        })
        .cloned()
        .collect()
}

/// The packages a `Cargo.lock` pins from a registry or git (`source = …`),
/// by name and version, with the checksum it records for each, if any.
fn pinned(lock: &str) -> BTreeMap<(String, String), Option<String>> {
    let mut packages: BTreeMap<usize, BTreeMap<&str, String>> = BTreeMap::new();
    for entry in entries(lock) {
        if entry.table == "package" && entry.quoted {
            packages
                .entry(entry.header)
                .or_default()
                .insert(entry.key, entry.value);
        }
    }
    packages
        .into_values()
        .filter(|package| package.contains_key("source"))
        .filter_map(|mut package| {
            Some((
                (package.remove("name")?, package.remove("version")?),
                package.remove("checksum"),
            ))
        })
        .collect()
}

/// One `key = value` line of a TOML file, read line by line without a TOML
/// parser: the table it is in (its header's name, without brackets, spaces
/// or a comment after it), which header that is (from 1, so two
/// `[[package]]` entries differ), the key unquoted, and the value without a
/// comment after it, a string unquoted (`quoted`).
struct Entry<'a> {
    table: String,
    header: usize,
    key: &'a str,
    value: String,
    quoted: bool,
}

fn entries(text: &str) -> Vec<Entry<'_>> {
    let mut found = Vec::new();
    let mut table = String::new();
    let mut header = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            let name = line.trim_start_matches('[');
            let name = name.split(']').next().unwrap_or("").trim();
            table = name.trim_matches(['"', '\'']).to_string();
            header += 1;
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches(['"', '\'']);
        let value = value.trim();
        let quote = value.chars().next().filter(|c| matches!(c, '"' | '\''));
        let (value, quoted) = match quote {
            Some(quote) => match value[1..].find(quote) {
                Some(end) => (value[1..1 + end].to_string(), true),
                None => continue,
            },
            None => (
                value.split('#').next().unwrap_or("").trim().to_string(),
                false,
            ),
        };
        found.push(Entry {
            table: table.clone(),
            header,
            key,
            value,
            quoted,
        });
    }
    found
}

/// Whether `path` is one of `paths` or under one of them, looked up by each
/// of its ancestors.
fn under(paths: &BTreeSet<PathBuf>, path: &Path) -> bool {
    !paths.is_empty() && path.ancestors().any(|ancestor| paths.contains(ancestor))
}

/// What a manifest says of its package's build script (`package.build`):
/// none said, so `build.rs` beside it if there is one; a path from it; or
/// none at all (`false`). A `build` key in the `[package]` table, or
/// `package.build` before any table (`entries`).
enum Build {
    Default,
    Path(String),
    Off,
}

fn build_setting(manifest: &str) -> Build {
    for entry in entries(manifest) {
        let build = (matches!(entry.table.as_str(), "package" | "project") && entry.key == "build")
            || (entry.table.is_empty() && entry.key == "package.build");
        if !build {
            continue;
        }
        return match (entry.quoted, entry.value.as_str()) {
            (true, path) => Build::Path(path.to_string()),
            (false, "false") => Build::Off,
            (false, _) => Build::Default,
        };
    }
    Build::Default
}

/// Every path a string literal in the build script `text` names, from its
/// package's directory `package`, into `named`: each run of path characters
/// in a literal's value (so `"cargo:rerun-if-changed=schema/a.json"` names
/// `schema/a.json`, and `"{}/proto/{}.proto"` names `proto`), without the
/// separators it starts with (a piece joined onto a directory), resolved
/// lexically. Neither the package's directory itself nor one above it is
/// named: a build script that reads all of it reads by a path the scan does
/// not see.
fn build_named(text: &str, package: &Path, named: &mut BTreeSet<PathBuf>) {
    let path = |c: char| {
        c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '@' | '~' | '/' | '\\')
    };
    for (_, literal) in scan(text).literals {
        let unprefixed = literal.strip_prefix(['b', 'c']).unwrap_or(literal);
        let value = string_literal(unprefixed).unwrap_or_else(|| literal.to_string());
        for run in value.split(|c: char| !path(c)) {
            let run = run.replace('\\', "/");
            let run = run.trim_start_matches('/');
            if run.is_empty() {
                continue;
            }
            let Some(resolved) = lexical(&package.join(run)) else {
                continue;
            };
            if !package.starts_with(&resolved) {
                named.insert(resolved);
            }
        }
    }
}

/// What a set of Rust sources read beyond Cargo's own files: the files they
/// include, and every path their string literals name.
#[derive(Default)]
pub(crate) struct Sources {
    included: BTreeSet<PathBuf>,
    opaque: Vec<String>,
    /// The directories of vendored packages, read whole (`Reads`): an
    /// include in one whose path cannot be known is not refused.
    vendored: BTreeSet<PathBuf>,
    /// File names a string literal names alone (`"README.md"`), with where
    /// the first such literal is (`src/x.rs:LINE`).
    bare: HashMap<String, String>,
    /// Paths a string literal names, and their endings of two names or more,
    /// with where the first such literal is.
    paths: HashMap<String, String>,
}

impl Sources {
    /// Read one source: `source` is its path as the caller spells paths
    /// (relative to the current directory, or absolute), `shown` how a
    /// refusal names it.
    pub(crate) fn read(&mut self, source: &Path, text: &str, shown: &str) {
        let base = source.parent().unwrap_or_else(|| Path::new(""));
        // A vendored package's files are all required, so what one of its
        // includes reads is, whatever its path.
        let vendored = under(&self.vendored, source);
        let Scanned {
            includes, literals, ..
        } = scan(text);
        for (line, literal) in literals {
            index(
                literal,
                &format!("{shown}:{line}"),
                &mut self.bare,
                &mut self.paths,
            );
        }
        for (line, include) in includes {
            let resolved = match include {
                Include::Source(path) => {
                    let named = Path::new(&path);
                    if named.is_absolute() {
                        lexical(named)
                    } else {
                        lexical(&base.join(named))
                    }
                }
                Include::Manifest(path) => {
                    // `concat!(env!("CARGO_MANIFEST_DIR"), "/x")`: from the
                    // directory of the nearest manifest above the source.
                    let manifest = base
                        .ancestors()
                        .find(|directory| directory.join("Cargo.toml").is_file());
                    match (manifest, path.strip_prefix(['/', '\\'])) {
                        (Some(manifest), Some(rest)) => lexical(&manifest.join(rest)),
                        _ if vendored => continue,
                        _ => {
                            self.opaque.push(format!(
                                "{shown}:{line} (a path from CARGO_MANIFEST_DIR that does not \
                                 start with / under a Cargo.toml)"
                            ));
                            continue;
                        }
                    }
                }
                Include::Opaque if vendored => continue,
                Include::Opaque => {
                    self.opaque
                        .push(format!("{shown}:{line} (the path is not a string literal)"));
                    continue;
                }
            };
            // A path that climbs above where the source's path starts is
            // outside what can be declared.
            if let Some(resolved) = resolved {
                self.included.insert(resolved);
            }
        }
    }

    /// The files the sources include, resolved as rustc resolves them: from
    /// the directory of the file that names them (from its manifest's for
    /// `CARGO_MANIFEST_DIR`), lexically, spelled as the sources were.
    #[cfg(test)]
    pub(crate) fn included(&self) -> &BTreeSet<PathBuf> {
        &self.included
    }

    /// Where a string literal in a source names `relative` (a path with `/`
    /// separators), as `src/x.rs:LINE`, as a test that reads a document does
    /// (`fs::read("README.md")`), and as a help or message text that names one
    /// does too: so a document found this way only ever warns. A comment or a
    /// doc comment that mentions a document names nothing. A file at the top
    /// level is named by its file name alone (`"README.md"`, `"./README.md"`,
    /// `"/README.md"` after `CARGO_MANIFEST_DIR`), never by a deeper path that
    /// ends in it; a deeper one by its path (`"docs/guide.md"`), or a path
    /// that ends in it. A path built from pieces is not seen.
    pub(crate) fn named_at(&self, relative: &str) -> Option<&str> {
        if relative.contains('/') {
            self.paths.get(relative)
        } else {
            self.bare.get(relative)
        }
        .map(String::as_str)
    }

    /// Why the files the sources read cannot be known, if they cannot: an
    /// include whose path is not a string literal (a macro's variable, an
    /// expression), outside the forms `read` resolves, cannot be required,
    /// so it is refused rather than passed over.
    pub(crate) fn refusal(&self) -> Option<String> {
        (!self.opaque.is_empty()).then(|| {
            format!(
                "cannot tell which file an include reads at {}; write its path as a string \
                 literal, or as concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/path\"), so the file can \
                 be required as an input (paths built from OUT_DIR, made at build time, are \
                 not asked for)",
                self.opaque.join(", ")
            )
        })
    }
}

/// What `scan` finds in a source, each with its line (from 1): the include
/// invocations, and the string literals in code, as written.
struct Scanned<'a> {
    includes: Vec<(usize, Include)>,
    literals: Vec<(usize, &'a str)>,
}

/// What one include invocation names.
#[derive(Debug, PartialEq)]
enum Include {
    /// A string literal: a path from the including file's directory.
    Source(String),
    /// `concat!(env!("CARGO_MANIFEST_DIR"), "...", ...)`: from the
    /// manifest's directory.
    Manifest(String),
    /// Anything else, which names no file that can be known.
    Opaque,
}

/// Every `include_str!`, `include_bytes!` and `include!` invocation in the
/// Rust source `text`, with its line. Code is read as Rust is: string and
/// character literals and comments are not code, so an invocation written
/// inside one is not one. Doc comments are read as rustdoc reads them
/// (`Docs`): the invocations in their code blocks count, since a doctest
/// compiles, and their prose, inline code included, names no file. An
/// argument built from `OUT_DIR` names a file made at build time, which is
/// not in git and is not returned. Also every string literal in code, as
/// written, with its line, for `index`.
fn scan(text: &str) -> Scanned<'_> {
    let bytes = text.as_bytes();
    // The line at a position, counted on from the last position asked
    // about, since positions are asked about in order.
    let mut counted = (0usize, 1usize);
    let mut line = |at: usize| {
        counted.1 += text[counted.0..at]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        counted.0 = at;
        counted.1
    };
    let mut found = Vec::new();
    let mut literals = Vec::new();
    let mut docs = Docs::default();
    let mut at = 0;
    while at < bytes.len() {
        let rest = &text[at..];
        if rest.starts_with("//") {
            let end = rest.find('\n').map_or(text.len(), |end| at + end);
            if rest.starts_with("///") && !rest.starts_with("////") {
                docs.comment(text, at, end, Kind::Outer, [&text[at + 3..end]]);
            } else if rest.starts_with("//!") {
                docs.comment(text, at, end, Kind::Inner, [&text[at + 3..end]]);
            }
            at = end;
            continue;
        }
        if rest.starts_with("/*") {
            let end = block_comment_end(text, at);
            let outer =
                rest.starts_with("/**") && !rest.starts_with("/***") && !rest.starts_with("/**/");
            if outer || rest.starts_with("/*!") {
                let inner = text[at + 3..end]
                    .strip_suffix("*/")
                    .unwrap_or(&text[at + 3..end]);
                let kind = if outer { Kind::Outer } else { Kind::Inner };
                docs.comment(text, at, end, kind, inner.split('\n').map(undecorated));
            }
            at = end;
            continue;
        }
        if let Some(end) = literal_end(text, at) {
            literals.push((line(at), &text[at..end]));
            at = end;
            continue;
        }
        if bytes[at] == b'\'' {
            at = quote_end(text, at);
            continue;
        }
        if identifier_start(bytes[at]) {
            let start = at;
            while at < bytes.len() && identifier(bytes[at]) {
                at += 1;
            }
            if matches!(
                &text[start..at],
                "include" | "include_str" | "include_bytes"
            ) {
                if let Some(include) = invocation(text, at) {
                    found.push((line(start), include));
                }
            }
            continue;
        }
        at += rest.chars().next().map_or(1, char::len_utf8);
    }
    // A doctest's includes. Their line is not kept, and one whose path is
    // not a literal is passed over: a doc comment is never refused.
    docs.flush();
    found.extend(docs.found.into_iter().map(|include| (0, include)));
    Scanned {
        includes: found,
        literals,
    }
}

/// Which item a doc comment documents: the one after it (`///`, `/**`) or
/// the one around it (`//!`, `/*!`).
#[derive(Clone, Copy)]
enum Kind {
    Outer,
    Inner,
}

/// A line of a block doc comment without the ` * ` that decorates it; a
/// line without one keeps its indentation, so an indented block stays
/// indented.
fn undecorated(line: &str) -> &str {
    let trimmed = line.trim_start();
    match trimmed.strip_prefix('*') {
        Some(rest) if !trimmed.starts_with("*/") => rest,
        _ => line,
    }
}

/// The includes of a source's doc comments, as rustdoc reads Markdown: in
/// the lines of a fenced code block (unless its info string names another
/// language) and in indented lines, and in nothing of their prose, inline
/// code included. rustdoc compiles a doc comment's code blocks as doctests,
/// and nothing else in it.
///
/// A block's includes are found by their text, each on its own (`textual`),
/// as every literal include in a doc comment once was, not by lexing the
/// block: a block need not be Rust as written (a `.gitignore` sample's
/// `/*`, an `ignore` block's unterminated string), and lexed, one such
/// block hid the includes after it. So an include in a doctest's comment or
/// string counts too.
///
/// Where the structure is unclear, every reading is kept, and a line any
/// reading takes for code is code, so a doctest's include is never missed
/// (the cost is a file asked for that nothing reads). Only the plainest
/// fence is read with certainty: three or more backticks or tildes, at the
/// start of a line indented by fewer than four columns, a backtick fence's
/// info string without a backtick, as CommonMark has it. Every other fence,
/// one after a list item's marker or a block quote's `>`, or indented by
/// four columns or more, opens or closes in one reading and not in another;
/// and a doc comment that does not follow the last one directly (code, an
/// attribute or a plain comment between them) may document the same item,
/// so a fence still open there stays open in one reading and is closed in
/// another.
#[derive(Default)]
struct Docs {
    outer: Readings,
    inner: Readings,
    /// The run of code lines read so far.
    run: String,
    found: Vec<Include>,
}

/// The readings of one kind of doc comment so far, and where the last one
/// ended.
#[derive(Default)]
struct Readings {
    readings: Vec<Reading>,
    last: Option<usize>,
}

#[derive(Clone, Copy, PartialEq)]
enum Reading {
    Prose,
    /// Inside a fenced block opened by `len` of `fence`; `rust` when
    /// rustdoc compiles it; `plain` when it opened at the start of a line
    /// indented by fewer than four columns, so a plain closing fence closes
    /// it surely.
    Fence {
        fence: u8,
        len: usize,
        rust: bool,
        plain: bool,
    },
}

impl Docs {
    /// The doc comment at `at..end` of `text`, of `kind`, as `lines`.
    fn comment<'a>(
        &mut self,
        text: &str,
        at: usize,
        end: usize,
        kind: Kind,
        lines: impl IntoIterator<Item = &'a str>,
    ) {
        let follows = {
            let state = self.state(kind);
            state
                .last
                .is_some_and(|last| text[last..at].trim().is_empty())
        };
        if !follows {
            let state = self.state(kind);
            if !state.readings.contains(&Reading::Prose) {
                state.readings.push(Reading::Prose);
            }
            // Nothing joins code across doc comments that do not follow
            // each other.
            self.flush();
        }
        for line in lines {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let code = self.state(kind).read(line);
            if code {
                self.run.push_str(line);
                self.run.push('\n');
            } else {
                self.flush();
            }
        }
        self.state(kind).last = Some(end);
    }

    fn state(&mut self, kind: Kind) -> &mut Readings {
        match kind {
            Kind::Outer => &mut self.outer,
            Kind::Inner => &mut self.inner,
        }
    }

    /// The includes of the run of code lines so far, found by their text.
    fn flush(&mut self) {
        textual(&self.run, &mut self.found);
        self.run.clear();
    }
}

impl Readings {
    /// Read one line of a doc comment: whether any reading takes it for
    /// code rustdoc compiles.
    fn read(&mut self, line: &str) -> bool {
        let indent = line
            .chars()
            .take_while(|c| matches!(c, ' ' | '\t'))
            .map(|c| if c == '\t' { 4 } else { 1 })
            .sum::<usize>();
        let trimmed = line.trim_start();
        // A fence at the start of the line, and one after a list item's
        // marker or a block quote's `>`, as rustdoc reads Markdown.
        let plain = fence_of(trimmed);
        let contained = fence_of(uncontained(trimmed)).filter(|_| plain.is_none());
        let sure = plain.is_some() && indent < 4;
        let mut code = false;
        let mut next = Vec::new();
        for reading in &self.readings {
            match *reading {
                Reading::Prose => {
                    if let Some((fence, len, info)) = plain.or(contained) {
                        next.push(Reading::Fence {
                            fence,
                            len,
                            rust: compiled(info),
                            plain: sure,
                        });
                    }
                    if !sure {
                        code |= indent >= 4 && !trimmed.is_empty();
                        next.push(Reading::Prose);
                    }
                }
                Reading::Fence {
                    fence: open,
                    len: opened,
                    rust,
                    plain: opened_plainly,
                } => {
                    let closes = plain.or(contained).is_some_and(|(fence, len, info)| {
                        fence == open && len >= opened && info.is_empty()
                    });
                    if closes {
                        next.push(Reading::Prose);
                    }
                    if !(closes && sure && opened_plainly) {
                        code |= rust;
                        next.push(*reading);
                    }
                }
            }
        }
        self.readings.clear();
        for reading in next {
            if !self.readings.contains(&reading) {
                self.readings.push(reading);
            }
        }
        code
    }
}

/// Every literal include invocation in `text` found by its text alone,
/// whatever surrounds it: a fallback for code that does not lex.
fn textual(text: &str, found: &mut Vec<Include>) {
    let mut at = 0;
    while let Some(offset) = text[at..].find("include") {
        let start = at + offset;
        at = start + "include".len();
        if start > 0 && identifier(text.as_bytes()[start - 1]) {
            continue;
        }
        let mut end = at;
        while end < text.len() && identifier(text.as_bytes()[end]) {
            end += 1;
        }
        if !matches!(
            &text[start..end],
            "include" | "include_str" | "include_bytes"
        ) {
            continue;
        }
        match invocation(text, end) {
            Some(Include::Opaque) | None => {}
            Some(include) => found.push(include),
        }
    }
}

/// `trimmed` without the list item markers (`-`, `*`, `+`, `1.`, `1)`) and
/// block quote markers (`>`) it starts with.
fn uncontained(trimmed: &str) -> &str {
    let mut rest = trimmed;
    loop {
        rest = rest.trim_start();
        if let Some(quoted) = rest.strip_prefix('>') {
            rest = quoted;
            continue;
        }
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let marker = if (1..=9).contains(&digits) && rest[digits..].starts_with(['.', ')']) {
            digits + 1
        } else if rest.starts_with(['-', '*', '+']) {
            1
        } else {
            0
        };
        if marker > 0 && rest[marker..].starts_with([' ', '\t']) {
            rest = &rest[marker..];
            continue;
        }
        return rest;
    }
}

/// The fence a line of a doc comment opens or closes, with leading
/// whitespace removed: its character, its length (three or more) and its
/// info string. A run of backticks whose info string holds a backtick is
/// no fence, as CommonMark has it: "```sh``` blocks" is inline code.
fn fence_of(trimmed: &str) -> Option<(u8, usize, &str)> {
    let fence = *trimmed.as_bytes().first()?;
    if !matches!(fence, b'`' | b'~') {
        return None;
    }
    let len = trimmed.bytes().take_while(|byte| *byte == fence).count();
    let info = trimmed[len..].trim();
    (len >= 3 && !(fence == b'`' && info.contains('`'))).then_some((fence, len, info))
}

/// Whether rustdoc compiles a fenced block with this info string: unless it
/// names another language and not `rust` as well. Words rustdoc reads as
/// test attributes (`no_run`, `ignore`, `should_panic`, an edition) and
/// unknown ones count as Rust here, so what a doctest includes is never
/// missed.
fn compiled(info: &str) -> bool {
    const OTHER: &[&str] = &[
        "text",
        "txt",
        "plain",
        "plaintext",
        "console",
        "sh",
        "shell",
        "bash",
        "zsh",
        "fish",
        "powershell",
        "ps1",
        "bat",
        "cmd",
        "toml",
        "json",
        "jsonc",
        "yaml",
        "yml",
        "ini",
        "md",
        "markdown",
        "html",
        "xml",
        "css",
        "c",
        "cpp",
        "c++",
        "h",
        "python",
        "py",
        "js",
        "javascript",
        "ts",
        "typescript",
        "go",
        "java",
        "diff",
        "sql",
        "dot",
        "mermaid",
        "csv",
    ];
    let words = info
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    words.iter().any(|word| word == "rust")
        || !words.iter().any(|word| OTHER.contains(&word.as_str()))
}

/// What the invocation whose macro name ends at `at` names, or None when
/// the name is not invoked there (`use std::include_str;`), or names a
/// file made at build time.
fn invocation(text: &str, at: usize) -> Option<Include> {
    let at = skip_space(text, at);
    let at = skip_space(text, at + text[at..].strip_prefix('!').map(|_| 1)?);
    let at = skip_space(
        text,
        at + text[at..].strip_prefix(['(', '[', '{']).map(|_| 1)?,
    );
    if let Some((path, _)) = string_at(text, at) {
        return Some(Include::Source(path));
    }
    match concatenated(text, at) {
        Some(named) => named,
        None => Some(Include::Opaque),
    }
}

/// What a `concat!` at `at` names: `concat!(env!("CARGO_MANIFEST_DIR"),
/// "...", ...)` a path from the manifest's directory, a `concat!` of
/// literals a path from the source's, and `concat!(env!("OUT_DIR"), ...)`
/// nothing (`Some(None)`). None for any other argument.
fn concatenated(text: &str, at: usize) -> Option<Option<Include>> {
    let at = text[at..]
        .strip_prefix("concat")
        .map(|_| at + "concat".len())?;
    let at = skip_space(text, at);
    let at = skip_space(text, at + text[at..].strip_prefix('!').map(|_| 1)?);
    let mut at = skip_space(
        text,
        at + text[at..].strip_prefix(['(', '[', '{']).map(|_| 1)?,
    );
    let mut from = None;
    if let Some(after) = text[at..].strip_prefix("env") {
        let next = skip_space(text, text.len() - after.len());
        let next = skip_space(text, next + text[next..].strip_prefix('!').map(|_| 1)?);
        let next = skip_space(
            text,
            next + text[next..].strip_prefix(['(', '[', '{']).map(|_| 1)?,
        );
        let (variable, next) = string_at(text, next)?;
        let next = skip_space(text, next);
        let next = skip_space(
            text,
            next + text[next..].strip_prefix([')', ']', '}']).map(|_| 1)?,
        );
        match variable.as_str() {
            // Made at build time: not in git, never an input.
            "OUT_DIR" => return Some(None),
            "CARGO_MANIFEST_DIR" => from = Some(variable),
            _ => return None,
        }
        at = next;
        at = skip_space(text, at + text[at..].strip_prefix(',').map(|_| 1)?);
    }
    let mut path = String::new();
    loop {
        let (piece, next) = string_at(text, at)?;
        path.push_str(&piece);
        let next = skip_space(text, next);
        if let Some(after) = text[next..].strip_prefix(',') {
            at = skip_space(text, text.len() - after.len());
            if text[at..].starts_with([')', ']', '}']) {
                break;
            }
        } else if text[next..].starts_with([')', ']', '}']) {
            break;
        } else {
            return None;
        }
    }
    Some(Some(match from {
        Some(_) => Include::Manifest(path),
        None => Include::Source(path),
    }))
}

/// `at` past whitespace and comments.
fn skip_space(text: &str, mut at: usize) -> usize {
    loop {
        let rest = &text[at..];
        let trimmed = rest.trim_start();
        at += rest.len() - trimmed.len();
        if trimmed.starts_with("//") {
            at = trimmed.find('\n').map_or(text.len(), |end| at + end);
        } else if trimmed.starts_with("/*") {
            at = block_comment_end(text, at);
        } else {
            return at;
        }
    }
}

/// Where the block comment opening at `at` ends, nested ones included.
fn block_comment_end(text: &str, at: usize) -> usize {
    let mut depth = 0usize;
    let mut index = at;
    while index < text.len() {
        let rest = &text[index..];
        if rest.starts_with("/*") {
            depth += 1;
            index += 2;
        } else if rest.starts_with("*/") {
            depth -= 1;
            index += 2;
            if depth == 0 {
                return index;
            }
        } else {
            index += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    text.len()
}

/// The string literal at `at` (`"..."`, `r"..."`, `r#"..."#`) with where it
/// ends, unescaped; None for anything else.
fn string_at(text: &str, at: usize) -> Option<(String, usize)> {
    let rest = &text[at..];
    let value = string_literal(rest)?;
    let end = literal_end(text, at)?;
    Some((value, end))
}

/// Where a string, byte string or C string literal starting at `at` ends,
/// raw ones included, or None when none starts there. A `b'x'` byte is
/// left to `quote_end`.
fn literal_end(text: &str, at: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if at > 0 && identifier(bytes[at - 1]) {
        return None;
    }
    let mut index = at;
    if matches!(bytes.get(index), Some(b'b' | b'c')) {
        index += 1;
    }
    let raw = bytes.get(index) == Some(&b'r');
    if raw {
        index += 1;
        let hashes = bytes[index..]
            .iter()
            .take_while(|byte| **byte == b'#')
            .count();
        index += hashes;
        if bytes.get(index) != Some(&b'"') {
            return None;
        }
        let close = format!("\"{}", "#".repeat(hashes));
        return Some(
            text[index + 1..]
                .find(&close)
                .map_or(text.len(), |end| index + 1 + end + close.len()),
        );
    }
    if bytes.get(index) != Some(&b'"') {
        return None;
    }
    index += 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return Some(index + 1),
            _ => index += 1,
        }
    }
    Some(text.len())
}

/// Where the character literal or lifetime starting with the `'` at `at`
/// ends: `'x'` and `'\n'` are characters, `'a` in `&'a str` a lifetime.
fn quote_end(text: &str, at: usize) -> usize {
    let rest = &text[at + 1..];
    let mut chars = rest.char_indices();
    match chars.next() {
        Some((_, '\\')) => rest[1..]
            .char_indices()
            .skip(1)
            .find(|(_, c)| *c == '\'')
            .map_or(text.len(), |(end, _)| at + 2 + end + 1),
        // A lifetime's name is read as an identifier after its quote.
        Some(_) => match chars.next() {
            Some((end, '\'')) => at + 1 + end + 1,
            _ => at + 1,
        },
        None => text.len(),
    }
}

fn identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn identifier(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Every run of path characters in the string literal `literal` (letters,
/// digits, `.`, `_`, `-`, `+`, `@`, `~`, and `/` or `\`, either one a
/// separator), `/`-separated, without the `/`, `./` and `../` it starts
/// with: a single name into `bare`; a path, with each of its endings of two
/// names or more, into `paths`, each with `at`, where the literal is, unless
/// an earlier literal named it. So `"home/.claude/CLAUDE.md"` names
/// `.claude/CLAUDE.md` but never a top-level `CLAUDE.md`.
fn index(
    literal: &str,
    at: &str,
    bare: &mut HashMap<String, String>,
    paths: &mut HashMap<String, String>,
) {
    let path = |c: char| {
        c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '@' | '~' | '/' | '\\')
    };
    for run in literal
        .split(|c: char| !path(c))
        .filter(|run| !run.is_empty())
    {
        let run = run.replace("\\\\", "/").replace('\\', "/");
        let mut rest = run.as_str();
        loop {
            let before = rest;
            rest = rest.trim_start_matches('/');
            rest = rest.strip_prefix("./").unwrap_or(rest);
            rest = rest.strip_prefix("../").unwrap_or(rest);
            if rest == before {
                break;
            }
        }
        if rest.is_empty() || rest == "." || rest == ".." {
            continue;
        }
        if !rest.contains('/') {
            bare.entry(rest.to_string())
                .or_insert_with(|| at.to_string());
            continue;
        }
        let mut ending = rest;
        while ending.contains('/') {
            paths
                .entry(ending.trim_end_matches('/').to_string())
                .or_insert_with(|| at.to_string());
            match ending.find('/') {
                Some(separator) => ending = &ending[separator + 1..],
                None => break,
            }
        }
    }
}

/// The value of the string literal `text` starts with: `"..."` with Rust's
/// escapes, or a raw `r"..."`, `r#"..."#`. None for anything else, or a
/// literal that does not close.
fn string_literal(text: &str) -> Option<String> {
    if let Some(raw) = text.strip_prefix('r') {
        let hashes = raw.bytes().take_while(|byte| *byte == b'#').count();
        let body = raw[hashes..].strip_prefix('"')?;
        let close = format!("\"{}", "#".repeat(hashes));
        return body.find(&close).map(|end| body[..end].to_string());
    }
    let mut chars = text.strip_prefix('"')?.chars();
    let mut value = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(value),
            '\\' => match chars.next()? {
                'n' => value.push('\n'),
                'r' => value.push('\r'),
                't' => value.push('\t'),
                '0' => value.push('\0'),
                '\\' => value.push('\\'),
                '\'' => value.push('\''),
                '"' => value.push('"'),
                'x' => {
                    let hex: String = chars.by_ref().take(2).collect();
                    value.push(char::from(u8::from_str_radix(&hex, 16).ok()?));
                }
                'u' => {
                    if chars.next()? != '{' {
                        return None;
                    }
                    let hex: String = chars.by_ref().take_while(|c| *c != '}').collect();
                    let code = u32::from_str_radix(&hex.replace('_', ""), 16).ok()?;
                    value.push(char::from_u32(code)?);
                }
                // A line continuation: the newline and the whitespace after it.
                '\n' | '\r' => {
                    let rest = chars.as_str().trim_start();
                    chars = rest.chars();
                }
                _ => return None,
            },
            c => value.push(c),
        }
    }
    None
}

/// `path` with `.` and `name/..` removed, without asking the file system.
/// None when a `..` climbs above where the path starts.
fn lexical(path: &Path) -> Option<PathBuf> {
    let mut resolved = PathBuf::new();
    let mut names = 0usize;
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if names == 0 {
                    return None;
                }
                resolved.pop();
                names -= 1;
            }
            Component::Normal(name) => {
                resolved.push(name);
                names += 1;
            }
            Component::RootDir | Component::Prefix(_) => resolved.push(component.as_os_str()),
        }
    }
    Some(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn included(source: &str, text: &str) -> (Vec<String>, Option<String>) {
        let mut sources = Sources::default();
        sources.read(Path::new(source), text, source);
        let named = sources
            .included()
            .iter()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect();
        (named, sources.refusal())
    }

    #[test]
    fn each_include_macro_names_a_file_beside_its_source() {
        let text = r##"
            const A: &str = include_str!("launch.zsh");
            static B: &[u8] = include_bytes!("../assets/logo.png");
            include!("generated.in");
            #![doc = std::include_str!("../OVERVIEW.md")]
            const C: &str = core::include_str! ( r#"raw "name".txt"# );
            const D: &str = include_str![r"brackets.txt"];
            const E: &str = include_str!{"braces.txt"};
            const F: &str = include_str!(/* why */ "commented.txt" // and how
            );
            const G: &str = include_str!(concat!("pie", "ces.txt"));
            /// A doctest:
            ///
            /// ```
            /// include_str!("doctest.txt");
            /// ```
            ///
            /// and prose about include_str!(path).
            fn documented() {}
        "##;
        let (named, refusal) = included("src/main.rs", text);
        assert_eq!(refusal, None);
        let mut expected = vec![
            "src/launch.zsh",
            "assets/logo.png",
            "src/generated.in",
            "OVERVIEW.md",
            "src/raw \"name\".txt",
            "src/brackets.txt",
            "src/braces.txt",
            "src/commented.txt",
            "src/pieces.txt",
            "src/doctest.txt",
        ];
        expected.sort();
        assert_eq!(named, expected);
    }

    #[test]
    fn strings_and_comments_hold_no_invocations() {
        let text = concat!(
            "const S: &str = \"include_str!(\\\"in-a-string.txt\\\")\";\n",
            "const R: &str = r#\"include_str!(path) and include_str!(\"raw.txt\")\"#;\n",
            "// include_str!(\"in-a-comment.txt\") and include_str!(path)\n",
            "/* include_str!(\"in-a-block.txt\") /* nested include_str!(x) */ */\n",
            "fn f<'a>(s: &'a str) -> char { let _ = s; '\"' }\n",
            "use std::include_str;\n",
            "fn my_include_str() {}\n",
            "const T: &str = include_str!(\"after.txt\");\n",
        );
        assert_eq!(
            included("src/lib.rs", text),
            (vec!["src/after.txt".into()], None)
        );
    }

    /// rustdoc compiles a doc comment's code blocks and nothing else: a
    /// fenced block (unless its info string names another language) or an
    /// indented one. Where a doc comment's structure is unclear (a fence
    /// left open before the next item's doc), every reading counts, so what
    /// a doctest includes is never missed. A block's includes are found by
    /// their text, so one in a doctest's comment counts as well.
    #[test]
    fn a_doc_comment_counts_the_includes_of_its_code_blocks_only() {
        let text = concat!(
            "/// Prose: `include_str!(\"inline.txt\")` and include_str!(\"sentence.txt\").\n",
            "///\n",
            "/// ```\n",
            "/// let a = include_str!(\n",
            "///     \"split.txt\",\n",
            "/// );\n",
            "/// // include_str!(\"commented.txt\")\n",
            "/// ```\n",
            "///\n",
            "/// ```rust,no_run\n",
            "/// include_str!(\"no-run.txt\");\n",
            "/// ```\n",
            "/// ~~~ignore\n",
            "/// # include_str!(\"ignored.txt\");\n",
            "/// ~~~\n",
            "/// ```text\n",
            "/// include_str!(\"text.txt\");\n",
            "/// ```\n",
            "/// ````sh\n",
            "/// ```\n",
            "/// include_str!(\"sh.txt\");\n",
            "/// ````\n",
            "///\n",
            "///     include_str!(\"indented.txt\");\n",
            "#[inline]\n",
            "fn f() {}\n",
            "/** Block prose, include_str!(\"block-prose.txt\").\n",
            " *\n",
            " * ```\n",
            " * include_str!(\"block.txt\");\n",
            " * ```\n",
            " */\n",
            "fn g() {}\n",
            "//! Inner prose, `include_str!(\"inner-prose.txt\")`.\n",
            "//! ```\n",
            "//! include_str!(\"inner.txt\");\n",
            "//! ```\n",
            "/// ```\n",
            "/// include_str!(\"unclosed.txt\");\n",
            "fn h() {}\n",
            "/// ```\n",
            "/// include_str!(\"after.txt\");\n",
            "/// ```\n",
            "fn i() {}\n",
        );
        let (named, refusal) = included("src/lib.rs", text);
        assert_eq!(refusal, None);
        let mut expected = vec![
            "src/split.txt",
            // Found by its text, in a doctest's comment too, as on main.
            "src/commented.txt",
            "src/no-run.txt",
            "src/ignored.txt",
            "src/indented.txt",
            "src/block.txt",
            "src/inner.txt",
            "src/unclosed.txt",
            "src/after.txt",
        ];
        expected.sort();
        assert_eq!(named, expected);
    }

    /// A code block in a list item or a block quote is a doctest too, its
    /// fence after the item's marker or the quote's `>`.
    #[test]
    fn a_code_block_in_a_list_or_a_quote_is_a_doctest() {
        let text = concat!(
            "/// - ```\n",
            "///   include_str!(\"listed.txt\");\n",
            "///   ```\n",
            "/// 1. ```\n",
            "///    include_str!(\"numbered.txt\");\n",
            "///    ```\n",
            "///\n",
            "/// > ```\n",
            "/// > include_str!(\"quoted.txt\");\n",
            "/// > ```\n",
            "fn f() {}\n",
        );
        let (named, refusal) = included("src/lib.rs", text);
        assert_eq!(refusal, None);
        assert_eq!(
            named,
            ["src/listed.txt", "src/numbered.txt", "src/quoted.txt"]
        );
    }

    /// Found on review: an earlier block that is not Rust as written (a
    /// `.gitignore` sample's `/*`, an `ignore` or `compile_fail` block's
    /// unterminated string, a regex's `"`) hid every later doctest's include,
    /// and a line that only shows a fence in inline code opened one, so the
    /// real fence after it read as a close. Each shape, before a doctest.
    #[test]
    fn a_block_that_is_not_rust_hides_no_later_doctest() {
        let doctest =
            "/// ```\n/// let data = include_str!(\"../fixtures/x.txt\");\n/// ```\nfn load() {}\n";
        for (shape, before) in [
            (
                "gitignore",
                "//! Add this to your `.gitignore`:\n//!\n//! ```gitignore\n//! /target/*\n//! ```\n\n",
            ),
            (
                "ignore, the same item",
                "/// ```ignore\n/// let s = \"unterminated;\n/// ```\n///\n",
            ),
            (
                "compile_fail, an earlier item",
                "/// ```compile_fail\n/// let s = 'x\n/// let t = \"unterminated;\n/// ```\nfn fails() {}\n",
            ),
            (
                "regex",
                "/// ```regex\n/// \"[a-z]+\n/// ```\nfn matches() {}\n",
            ),
            (
                "a fence shown inline",
                "/// A ```sh``` block is skipped.\n///\n",
            ),
            (
                "a block comment left open",
                "/// ```\n/// /* not closed\n/// ```\nfn open() {}\n",
            ),
        ] {
            let text = format!("{before}{doctest}");
            let (named, refusal) = included("src/lib.rs", &text);
            assert_eq!(refusal, None, "{shape}");
            assert_eq!(named, ["fixtures/x.txt"], "{shape}");
        }
    }

    /// Within one block whose code does not lex to its end, its includes
    /// are still found, by their text, as every literal include in a doc
    /// comment once was.
    #[test]
    fn a_block_that_does_not_lex_keeps_its_own_includes() {
        let text = concat!(
            "/// ```ignore\n",
            "/// let s = \"unterminated;\n",
            "/// let t = include_str!(\"in-the-block.txt\");\n",
            "/// ```\n",
            "fn f() {}\n",
        );
        assert_eq!(
            included("src/lib.rs", text),
            (vec!["src/in-the-block.txt".into()], None)
        );
    }

    #[test]
    fn escapes_are_unescaped() {
        let text = concat!(
            "include_str!(\"dir\\\\file\\x2etxt\");\n",
            "include_str!(\"a\\u{2F}b.txt\");\n",
            "include_str!(\"long\\\n     name.txt\");\n",
        );
        let (named, refusal) = included("src/lib.rs", text);
        assert_eq!(refusal, None);
        assert_eq!(named.len(), 3, "{named:?}");
        assert!(named.contains(&"src/a/b.txt".to_string()), "{named:?}");
        assert!(named.contains(&"src/longname.txt".to_string()), "{named:?}");
    }

    #[test]
    fn a_path_that_is_no_literal_is_refused_with_its_line_unless_made_at_build_time() {
        let generated = "include!(concat!(env!(\"OUT_DIR\"), \"/generated.rs\"));\n";
        assert_eq!(included("src/lib.rs", generated), (vec![], None));
        for (text, line) in [
            (
                "\n\nmacro_rules! t { ($p:literal) => { include_str!($p) }; }\n",
                3,
            ),
            ("const A: &str = include_str!(PATH);\n", 1),
            (
                "const A: &str = include_str!(concat!(env!(\"HOME\"), \"/x\"));\n",
                1,
            ),
            ("const A: &str = include_str!(concat!(\"a\", $x));\n", 1),
            ("\nconst A: &str = include_str!(path!());\n", 2),
        ] {
            let (named, refusal) = included("src/lib.rs", text);
            assert!(named.is_empty(), "{text}: {named:?}");
            let refusal = refusal.unwrap_or_else(|| panic!("{text}: no refusal"));
            assert!(
                refusal.contains(&format!(
                    "src/lib.rs:{line} (the path is not a string literal)"
                )),
                "{text}: {refusal}"
            );
        }
    }

    #[test]
    fn a_path_from_the_manifest_directory_resolves_from_the_nearest_manifest() {
        let root = std::env::temp_dir().join(format!(
            "sykli-manifest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("crates/a/src/bin")).unwrap();
        std::fs::write(root.join("crates/a/Cargo.toml"), "").unwrap();
        let text =
            "#![doc = include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/README\", \".md\"))]\n";
        let source = root.join("crates/a/src/bin/tool.rs");
        let (named, refusal) = included(source.to_str().unwrap(), text);
        assert_eq!(refusal, None);
        assert_eq!(
            named,
            [root
                .join("crates/a/README.md")
                .to_string_lossy()
                .replace('\\', "/")]
        );
        let text = "include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"README.md\"));\n";
        let (_, refusal) = included(source.to_str().unwrap(), text);
        assert!(refusal.is_some_and(|refusal| refusal.contains("does not start with /")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_path_climbing_out_of_the_tree_is_dropped_and_an_absolute_one_kept() {
        assert_eq!(
            included("src/lib.rs", r#"include_str!("../../outside.txt");"#),
            (vec![], None)
        );
        assert_eq!(
            included("/repo/src/lib.rs", r#"include_str!("./../data/./x.txt");"#).0,
            ["/repo/data/x.txt"]
        );
    }

    #[test]
    fn a_string_literal_names_a_top_level_file_by_its_name_and_a_deeper_one_by_its_path() {
        let mut sources = Sources::default();
        sources.read(
            Path::new("tests/docs.rs"),
            concat!(
                "//! The setup is in AGENTS.md and docs/setup.md.\n",
                "/// Reads README.md, as STYLE.md says.\n",
                "fn f() {\n",
                "    // CHANGELOG.md and docs/history.md are only mentioned here.\n",
                "    /* LICENSE, in a block comment */\n",
                "    fs::read(\"README.md\"); fs::read(\"docs/guide.md\");\n",
                "    let ci = root.join(\".github/workflows/ci.yml\");\n",
                "    include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/NOTICE\"));\n",
                "    fs::read(\"./COPYING\"); fs::read(\"docs\\\\windows.md\");\n",
                "    let elsewhere = \"home/.claude/CLAUDE.md\";\n",
                "}\n",
            ),
            "tests/docs.rs",
        );
        for named in [
            "README.md",
            "docs/guide.md",
            ".github/workflows/ci.yml",
            "workflows/ci.yml",
            "NOTICE",
            "COPYING",
            "docs/windows.md",
            ".claude/CLAUDE.md",
        ] {
            assert!(sources.named_at(named).is_some(), "{named}");
        }
        // Where the first literal that names it is.
        assert_eq!(sources.named_at("README.md"), Some("tests/docs.rs:6"));
        assert_eq!(sources.named_at("COPYING"), Some("tests/docs.rs:9"));
        for unnamed in [
            // Mentioned in a comment or a doc comment only.
            "AGENTS.md",
            "docs/setup.md",
            "STYLE.md",
            "CHANGELOG.md",
            "docs/history.md",
            "LICENSE",
            // A bare name is a top-level file's, never a deeper path's ending.
            "CLAUDE.md",
            "guide.md",
            "ci.yml",
            "other/docs/guide.md",
            "s/guide.md",
        ] {
            assert!(sources.named_at(unnamed).is_none(), "{unnamed}");
        }
    }

    #[test]
    fn documents_licences_and_ci_configuration_are_docs_at_any_depth() {
        for path in [
            "README.md",
            "crates/a/CHANGELOG.MD",
            "LICENSE",
            "LICENSE-APACHE",
            "vendor/dep/licence.txt",
            "COPYING",
            "NOTICE.txt",
            "docs/diagram.svg",
            "crates/a/docs/usage.txt",
            ".github/workflows/ci.yml",
        ] {
            assert!(docs(Path::new(path)), "{path}");
        }
        for path in [
            "src/lib.rs",
            "docs",
            "fixtures/case.json",
            "src/docsy/x.txt",
        ] {
            assert!(!docs(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn cargo_reads_these_by_name() {
        for path in [
            "src/lib.rs",
            "Cargo.toml",
            "vendor/dep/Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            "tests/fixtures/case.json",
            "crates/a/tests/data.txt",
        ] {
            assert!(by_name(Path::new(path)), "{path}");
        }
        for path in [
            "README.md",
            "src/launch.zsh",
            "fixtures/case.json",
            "config.toml",
        ] {
            assert!(!by_name(Path::new(path)), "{path}");
        }
    }

    /// A directory of files, made for one test and removed with it.
    struct Tree(PathBuf);

    impl Tree {
        fn new(tag: &str, files: &[(&str, &str)]) -> Tree {
            let root = std::env::temp_dir().join(format!(
                "sykli-reads-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            for (path, contents) in files {
                let path = root.join(path);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, contents).unwrap();
            }
            Tree(root)
        }

        /// What the build reads of every file in the tree.
        fn reads(&self, files: &[(&str, &str)]) -> (Reads, Vec<PathBuf>) {
            let listed = files
                .iter()
                .map(|(path, _)| self.0.join(path))
                .collect::<Vec<_>>();
            (Reads::of(&self.0, &listed).unwrap(), listed)
        }

        fn read(&self, reads: &Reads) -> Vec<String> {
            let mut read = walk(&self.0)
                .into_iter()
                .filter(|path| reads.reads(path))
                .map(|path| {
                    path.strip_prefix(&self.0)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/")
                })
                .collect::<Vec<_>>();
            read.sort();
            read
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn walk(directory: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(directory).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(walk(&path));
            } else {
                found.push(path);
            }
        }
        found
    }

    /// The package `cargo vendor` wrote, under the directory a tracked Cargo
    /// configuration replaces a source with, and pinned by `Cargo.lock`.
    const VENDORED: &[(&str, &str)] = &[
        ("Cargo.toml", "[package]\nname = \"x\"\n"),
        (
            "Cargo.lock",
            "version = 3\n\n[[package]]\nname = \"a\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"0a\"\n\n[[package]]\nname = \"x\"\nversion = \"0.1.0\"\n",
        ),
        (
            ".cargo/config.toml",
            "[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n[source.vendored-sources]\ndirectory = \"vendor\"\n",
        ),
        ("src/lib.rs", "pub fn f() {}\n"),
        (
            "vendor/a/Cargo.toml",
            "[package]\nname = \"a\"\nversion = \"1.0.0\"\n",
        ),
        (
            "vendor/a/.cargo-checksum.json",
            "{\"files\":{},\"package\":\"0a\"}",
        ),
        (
            "vendor/a/src/lib.rs",
            "macro_rules! t { ($p:expr) => { include_bytes!($p) }; }\n",
        ),
        ("vendor/a/data/x.bin", "x"),
        ("vendor/b/Cargo.toml", "[package]\nname = \"b\"\n"),
        ("vendor/b/src/lib.rs", "pub fn g() {}\n"),
        ("vendor/b/data/y.bin", "y"),
    ];

    /// Every file of a package with a `.cargo-checksum.json` beside its
    /// manifest is read; an include there whose path is no literal is not
    /// refused only when Cargo verifies that checksum, which the tree
    /// proves: the package is in a directory a tracked Cargo configuration
    /// names as a replaced source, and `Cargo.lock` pins it from a registry
    /// with the checksum the file records.
    #[test]
    fn a_vendored_package_is_read_whole() {
        let tree = Tree::new("vendored", VENDORED);
        let (reads, _) = tree.reads(VENDORED);
        assert_eq!(reads.sources().refusal(), None);
        let whole = [
            "vendor/a/.cargo-checksum.json",
            "vendor/a/Cargo.toml",
            "vendor/a/data/x.bin",
            "vendor/a/src/lib.rs",
        ];
        assert_eq!(tree.read(&reads), whole);
        let refused = |reads: &Reads| {
            reads
                .sources()
                .refusal()
                .is_some_and(|refusal| refusal.contains("vendor/a/src/lib.rs:1"))
        };
        // Without the proof, the package is still read whole, and its
        // include is refused: a checksum git does not list, a configuration
        // git does not list, a lock that does not pin it from a registry, a
        // checksum that is not the one pinned.
        let without = |left_out: &str| {
            VENDORED
                .iter()
                .filter(|(path, _)| *path != left_out)
                .copied()
                .collect::<Vec<_>>()
        };
        let (reads, _) = tree.reads(&without("vendor/a/.cargo-checksum.json"));
        assert!(refused(&reads), "{:?}", reads.sources().refusal());
        let (reads, _) = tree.reads(&without(".cargo/config.toml"));
        assert!(refused(&reads), "{:?}", reads.sources().refusal());
        assert_eq!(tree.read(&reads), whole);
        for (path, contents) in [
            // Pinned as a path package, as a patch of a vendored one is.
            (
                "Cargo.lock",
                "[[package]]\nname = \"a\"\nversion = \"1.0.0\"\n",
            ),
            // Another version.
            (
                "Cargo.lock",
                "[[package]]\nname = \"a\"\nversion = \"1.0.1\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"0a\"\n",
            ),
            // Another checksum.
            (
                "vendor/a/.cargo-checksum.json",
                "{\"files\":{},\"package\":\"0b\"}",
            ),
            // Another directory.
            (
                ".cargo/config.toml",
                "[source.vendored-sources]\ndirectory = \"third_party\"\n",
            ),
        ] {
            let mut changed = VENDORED.to_vec();
            for file in &mut changed {
                if file.0 == path {
                    file.1 = contents;
                }
            }
            let tree = Tree::new("unproven", &changed);
            let (reads, _) = tree.reads(&changed);
            assert!(refused(&reads), "{path}: {contents}");
            assert_eq!(tree.read(&reads), whole, "{path}: {contents}");
        }
    }

    /// Found on review: a `.cargo-checksum.json` beside a first-party
    /// manifest, planted or copied with a package out of a vendored
    /// directory, exempted that package's include. Cargo checks no such
    /// file outside a replaced source, so it proves nothing.
    #[test]
    fn a_checksum_beside_a_first_party_manifest_exempts_nothing() {
        let files = [
            ("Cargo.toml", "[workspace]\nmembers = [\"crates/a\"]\n"),
            ("crates/a/Cargo.toml", "[package]\nname = \"a\"\n"),
            ("crates/a/.cargo-checksum.json", "{\"files\":{}}"),
            (
                "crates/a/src/lib.rs",
                "macro_rules! data { ($p:expr) => { include_str!($p) } }\n",
            ),
            (
                "crates/a/build.rs",
                "fn main() { println!(\"cargo:rerun-if-changed=../../schema/x.json\"); }\n",
            ),
            ("schema/x.json", "{}"),
        ];
        let tree = Tree::new("planted", &files);
        let (reads, _) = tree.reads(&files);
        assert!(
            reads
                .sources()
                .refusal()
                .is_some_and(|refusal| refusal.contains("crates/a/src/lib.rs:1")),
            "{:?}",
            reads.sources().refusal()
        );
        assert!(tree.read(&reads).contains(&"schema/x.json".to_string()));
        assert_eq!(
            reads
                .build_scripts()
                .iter()
                .map(|script| script.strip_prefix(&tree.0).unwrap().to_path_buf())
                .collect::<Vec<_>>(),
            [PathBuf::from("crates/a/build.rs")]
        );
    }

    #[test]
    fn a_manifest_names_its_build_script_or_none() {
        for (manifest, expected) in [
            ("[package]\nname = \"x\"\n", "default"),
            (
                "[package]\nbuild = \"tools/build.rs\" # why\n",
                "tools/build.rs",
            ),
            ("[package]\nbuild = 'b.rs'\n", "b.rs"),
            ("[package]\nbuild = false\n", "off"),
            ("[package]\nbuild = true\n", "default"),
            ("package.build = \"top.rs\"\n[dependencies]\n", "top.rs"),
            ("[dependencies]\nbuild = \"1\"\n", "default"),
            ("[workspace]\nmembers = []\n", "default"),
            // A comment after a table's header hides nothing.
            (
                "[package] # the crate\nbuild = \"codegen.rs\"\n",
                "codegen.rs",
            ),
            ("[ package ]\nbuild = \"spaced.rs\"\n", "spaced.rs"),
        ] {
            let found = match build_setting(manifest) {
                Build::Default => "default".to_string(),
                Build::Path(path) => path,
                Build::Off => "off".to_string(),
            };
            assert_eq!(found, expected, "{manifest}");
        }
    }

    /// A build script's string literals name files and directories from its
    /// package's directory: a `cargo:rerun-if-changed` path, a path a read
    /// or a tool takes, a piece a path is joined from. A word that names no
    /// file reads nothing, and neither the package itself nor anything above
    /// it is named.
    #[test]
    fn a_build_script_s_literals_name_what_it_reads_from_its_package() {
        let files = [
            ("Cargo.toml", "[workspace]\nmembers = [\"crates/a\"]\n"),
            ("crates/a/Cargo.toml", "[package]\nname = \"a\"\n"),
            (
                "crates/a/build.rs",
                concat!(
                    "fn main() {\n",
                    "    println!(\"cargo::rerun-if-changed=schema/a.json\");\n",
                    "    println!(\"cargo:rerun-if-changed=proto\");\n",
                    "    let _ = std::fs::read(r\"templates\\b.txt\");\n",
                    "    let _ = format!(\"{}/gen/{}.in\", 1, 2);\n",
                    "    let _ = std::fs::read(\"../shared/common.txt\");\n",
                    "    let _ = std::fs::read_dir(\".\");\n",
                    "    let _ = std::fs::read(\"../..\");\n",
                    "    // \"notes.txt\" in a comment names nothing.\n",
                    "}\n",
                ),
            ),
            (
                "crates/a/src/lib.rs",
                "pub const S: &str = \"lib-named.txt\";\n",
            ),
            ("crates/a/schema/a.json", "{}"),
            ("crates/a/proto/x.proto", ""),
            ("crates/a/proto/sub/y.proto", ""),
            ("crates/a/templates/b.txt", ""),
            ("crates/a/gen/one.in", ""),
            ("crates/a/notes.txt", ""),
            ("crates/a/lib-named.txt", ""),
            ("crates/a/other.txt", ""),
            ("crates/shared/common.txt", ""),
            ("README.md", ""),
        ];
        let tree = Tree::new("build-script", &files);
        let (reads, _) = tree.reads(&files);
        assert_eq!(
            reads
                .build_scripts()
                .iter()
                .map(|script| script.strip_prefix(&tree.0).unwrap().to_path_buf())
                .collect::<Vec<_>>(),
            [PathBuf::from("crates/a/build.rs")]
        );
        assert_eq!(
            tree.read(&reads),
            [
                "crates/a/gen/one.in",
                "crates/a/proto/sub/y.proto",
                "crates/a/proto/x.proto",
                "crates/a/schema/a.json",
                "crates/a/templates/b.txt",
                "crates/shared/common.txt",
            ]
        );
    }
}
