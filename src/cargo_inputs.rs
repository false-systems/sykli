//! What a Cargo build reads, as `init` declares it and the completeness rule
//! refuses its absence (`validate`, `run`, `plan --explain`): Cargo's own
//! files, by name, and the files a Rust source names in an `include_str!`,
//! `include_bytes!` or `include!` string literal. One definition, so the
//! contract `init` writes is one the rule accepts.

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

/// The files the Rust source `source` names in `include_str!`,
/// `include_bytes!` and `include!` string literals, resolved as rustc
/// resolves them: from the directory of the file that names them, here
/// lexically. Every such literal in the text counts, in a comment too: a
/// file named that the build does not read costs a cache miss, a file read
/// but missed a false cached pass. A path a macro builds
/// (`concat!(env!("OUT_DIR"), ...)`) is no literal and is not found, nor is
/// a relative path that climbs above where `source`'s path starts.
pub(crate) fn included(source: &Path, text: &str) -> Vec<PathBuf> {
    let base = source.parent().unwrap_or_else(|| Path::new(""));
    literals(text)
        .into_iter()
        .filter_map(|literal| {
            let named = Path::new(&literal);
            if named.is_absolute() {
                lexical(named)
            } else {
                lexical(&base.join(named))
            }
        })
        .collect()
}

/// The string literal each `include_str!`, `include_bytes!` or `include!`
/// in `text` starts with, unescaped. A path qualifier (`std::`) and any
/// delimiter are allowed; an identifier that only ends in `include` is not
/// the macro.
fn literals(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(offset) = text[at..].find("include") {
        let start = at + offset;
        at = start + "include".len();
        let identifier = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
        if start > 0 && identifier(text.as_bytes()[start - 1]) {
            continue;
        }
        let mut rest = &text[at..];
        for suffix in ["_str", "_bytes"] {
            if let Some(after) = rest.strip_prefix(suffix) {
                rest = after;
                break;
            }
        }
        let Some(rest) = rest.trim_start().strip_prefix('!') else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix(['(', '[', '{']) else {
            continue;
        };
        if let Some(literal) = string_literal(rest.trim_start()) {
            found.push(literal);
        }
    }
    found
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

    fn named(source: &str, text: &str) -> Vec<String> {
        included(Path::new(source), text)
            .into_iter()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect()
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
            // include_str!("in-a-comment.txt") counts too
        "##;
        assert_eq!(
            named("src/main.rs", text),
            [
                "src/launch.zsh",
                "assets/logo.png",
                "src/generated.in",
                "OVERVIEW.md",
                "src/raw \"name\".txt",
                "src/brackets.txt",
                "src/braces.txt",
                "src/in-a-comment.txt",
            ]
        );
    }

    #[test]
    fn escapes_are_unescaped_and_a_built_path_is_not_a_literal() {
        let text = concat!(
            "include_str!(\"dir\\\\file\\x2etxt\");\n",
            "include_str!(\"a\\u{2F}b.txt\");\n",
            "include_str!(\"long\\\n     name.txt\");\n",
            "include_str!(concat!(env!(\"OUT_DIR\"), \"/out.rs\"));\n",
            "my_include_str!(\"not-the-macro.txt\");\n",
            "include_strs!(\"nor-this.txt\");\n",
            "include_str!(\"unclosed",
        );
        let named = named("src/lib.rs", text);
        assert_eq!(named.len(), 3, "{named:?}");
        assert!(named[0].ends_with("file.txt"), "{named:?}");
        assert_eq!(named[1], "src/a/b.txt");
        assert_eq!(named[2], "src/longname.txt");
    }

    #[test]
    fn a_path_climbing_out_of_the_tree_is_dropped_and_an_absolute_one_kept() {
        assert!(named("src/lib.rs", r#"include_str!("../../outside.txt")"#).is_empty());
        assert_eq!(
            named("/repo/src/lib.rs", r#"include_str!("./../data/./x.txt")"#),
            ["/repo/data/x.txt"]
        );
        #[cfg(unix)]
        assert_eq!(
            named("src/lib.rs", r#"include_str!("/etc/hostname")"#),
            ["/etc/hostname"]
        );
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
}
