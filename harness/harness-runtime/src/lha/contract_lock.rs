//! P1 Tree-Sitter public-interface contract lock for Rust and TypeScript.
//!
//! This is intentionally a best-effort semantic gate (R3). Compiler and test gates remain
//! authoritative for macros, reflection, FFI, generated code, and string-based references.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tree_sitter::{Language, Node, Parser};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContractLanguage {
    Rust,
    TypeScript,
    Tsx,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractEntry {
    pub path: PathBuf,
    pub language: ContractLanguage,
    pub kind: String,
    pub name: String,
    pub signature: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractSnapshot {
    pub entries: BTreeMap<String, ContractEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContractDiff {
    pub removed: Vec<String>,
    pub changed: Vec<String>,
    pub added: Vec<String>,
}

impl ContractDiff {
    pub fn compatible(&self) -> bool {
        self.removed.is_empty() && self.changed.is_empty()
    }
}

#[derive(Debug)]
pub enum ContractError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Parser(String),
    Syntax { path: PathBuf },
}

impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "contract lock I/O error: {error}"),
            Self::Json(error) => write!(f, "contract lock JSON error: {error}"),
            Self::Parser(error) => write!(f, "contract lock parser error: {error}"),
            Self::Syntax { path } => write!(f, "syntax error in {}", path.display()),
        }
    }
}

impl std::error::Error for ContractError {}

impl From<std::io::Error> for ContractError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for ContractError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

pub struct ContractLock;

impl ContractLock {
    pub fn capture(root: impl AsRef<Path>) -> Result<ContractSnapshot, ContractError> {
        let root = fs::canonicalize(root)?;
        let mut files = Vec::new();
        collect_sources(&root, &mut files)?;
        let mut snapshot = ContractSnapshot::default();
        for path in files {
            capture_file(&root, &path, &mut snapshot)?;
        }
        Ok(snapshot)
    }

    pub fn compare(baseline: &ContractSnapshot, candidate: &ContractSnapshot) -> ContractDiff {
        let mut diff = ContractDiff::default();
        for (key, entry) in &baseline.entries {
            match candidate.entries.get(key) {
                None => diff.removed.push(key.clone()),
                Some(next) if next.signature != entry.signature => diff.changed.push(key.clone()),
                Some(_) => {}
            }
        }
        for key in candidate.entries.keys() {
            if !baseline.entries.contains_key(key) {
                diff.added.push(key.clone());
            }
        }
        diff
    }

    pub fn save(snapshot: &ContractSnapshot, path: impl AsRef<Path>) -> Result<(), ContractError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        super::storage::atomic_write(path, &serde_json::to_vec_pretty(snapshot)?)?;
        Ok(())
    }

    pub fn load(path: impl AsRef<Path>) -> Result<ContractSnapshot, ContractError> {
        super::storage::recover_atomic(path.as_ref())?;
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }
}

fn collect_sources(directory: &Path, output: &mut Vec<PathBuf>) -> Result<(), ContractError> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name();
            if !matches!(
                name.to_str(),
                Some(".git" | "target" | "node_modules" | "dist")
            ) {
                collect_sources(&path, output)?;
            }
        } else if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("rs" | "ts" | "tsx")
        ) {
            output.push(path);
        }
    }
    output.sort();
    Ok(())
}

fn capture_file(
    root: &Path,
    path: &Path,
    snapshot: &mut ContractSnapshot,
) -> Result<(), ContractError> {
    let source = fs::read(path)?;
    let language = match path.extension().and_then(|value| value.to_str()) {
        Some("rs") => ContractLanguage::Rust,
        Some("tsx") => ContractLanguage::Tsx,
        _ => ContractLanguage::TypeScript,
    };
    let grammar: Language = match language {
        ContractLanguage::Rust => tree_sitter_rust::LANGUAGE.into(),
        ContractLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        ContractLanguage::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
    };
    let mut parser = Parser::new();
    parser
        .set_language(&grammar)
        .map_err(|error| ContractError::Parser(error.to_string()))?;
    let tree = parser
        .parse(&source, None)
        .ok_or_else(|| ContractError::Parser("parser returned no tree".into()))?;
    if tree.root_node().has_error() {
        return Err(ContractError::Syntax {
            path: path.to_path_buf(),
        });
    }
    let relative = path.strip_prefix(root).unwrap_or(path).to_path_buf();
    visit(
        tree.root_node(),
        &source,
        language,
        &relative,
        language == ContractLanguage::Rust,
        "",
        snapshot,
    );
    Ok(())
}

fn visit(
    node: Node<'_>,
    source: &[u8],
    language: ContractLanguage,
    path: &Path,
    exported: bool,
    namespace: &str,
    snapshot: &mut ContractSnapshot,
) {
    let kind = node.kind();
    let text = node_text(node, source);
    let now_exported = match language {
        ContractLanguage::Rust if kind == "mod_item" => exported && is_rust_public(text),
        ContractLanguage::Rust => exported,
        ContractLanguage::TypeScript | ContractLanguage::Tsx => {
            exported || kind == "export_statement"
        }
    };
    let capturable = match language {
        ContractLanguage::Rust => {
            exported
                && matches!(
                    kind,
                    "function_item"
                        | "struct_item"
                        | "enum_item"
                        | "trait_item"
                        | "type_item"
                        | "const_item"
                        | "static_item"
                        | "use_declaration"
                )
                && is_rust_public(text)
        }
        ContractLanguage::TypeScript | ContractLanguage::Tsx => {
            now_exported
                && matches!(
                    kind,
                    "function_declaration"
                        | "class_declaration"
                        | "interface_declaration"
                        | "type_alias_declaration"
                        | "enum_declaration"
                        | "lexical_declaration"
                )
        }
    };

    let standalone_ts_export = matches!(
        language,
        ContractLanguage::TypeScript | ContractLanguage::Tsx
    ) && kind == "export_statement"
        && !has_ts_declaration(node);
    if standalone_ts_export {
        let signature = canonical(node, source);
        let key = format!(
            "{}::{namespace}::{kind}::{signature}",
            path.to_string_lossy().replace('\\', "/")
        );
        snapshot.entries.insert(
            key,
            ContractEntry {
                path: path.to_path_buf(),
                language,
                kind: kind.into(),
                name: signature.clone(),
                signature,
            },
        );
        return;
    }

    if capturable {
        let name = node
            .child_by_field_name("name")
            .map(|value| node_text(value, source).to_owned())
            .unwrap_or_else(|| {
                let names = declaration_names(node, source);
                if names.is_empty() && kind == "use_declaration" {
                    canonical(node, source)
                } else {
                    names
                }
            });
        if !name.is_empty() {
            let key = format!(
                "{}::{namespace}::{kind}::{name}",
                path.to_string_lossy().replace('\\', "/")
            );
            snapshot.entries.insert(
                key,
                ContractEntry {
                    path: path.to_path_buf(),
                    language,
                    kind: kind.into(),
                    name,
                    signature: canonical(node, source),
                },
            );
        }
        return;
    }

    let next_namespace = match (language, kind) {
        (ContractLanguage::Rust, "impl_item") => node
            .child_by_field_name("type")
            .map(|value| qualify(namespace, node_text(value, source)))
            .unwrap_or_else(|| namespace.to_owned()),
        (ContractLanguage::Rust, "mod_item") => node
            .child_by_field_name("name")
            .map(|value| qualify(namespace, node_text(value, source)))
            .unwrap_or_else(|| namespace.to_owned()),
        _ => namespace.to_owned(),
    };
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit(
            child,
            source,
            language,
            path,
            now_exported,
            &next_namespace,
            snapshot,
        );
    }
}

fn has_ts_declaration(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|child| {
        matches!(
            child.kind(),
            "function_declaration"
                | "class_declaration"
                | "interface_declaration"
                | "type_alias_declaration"
                | "enum_declaration"
                | "lexical_declaration"
        )
    })
}

fn qualify(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        name.to_owned()
    } else {
        format!("{namespace}::{name}")
    }
}

fn is_rust_public(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("pub ") || trimmed.starts_with("pub(")
}

fn declaration_names(node: Node<'_>, source: &[u8]) -> String {
    let mut names = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "variable_declarator"
            && let Some(name) = child.child_by_field_name("name")
        {
            names.push(node_text(name, source).to_owned());
        }
    }
    names.join(",")
}

fn canonical(node: Node<'_>, source: &[u8]) -> String {
    fn tokens(node: Node<'_>, source: &[u8], output: &mut Vec<String>) {
        if matches!(node.kind(), "block" | "statement_block") {
            output.push("<body>".into());
            return;
        }
        if node.kind() == "comment" {
            return;
        }
        if node.child_count() == 0 {
            let value = node_text(node, source).trim();
            if !value.is_empty() {
                output.push(value.into());
            }
            return;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            tokens(child, source, output);
        }
    }
    let mut output = Vec::new();
    tokens(node, source, &mut output);
    output.join(" ")
}

fn node_text<'a>(node: Node<'_>, source: &'a [u8]) -> &'a str {
    std::str::from_utf8(&source[node.byte_range()]).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("lha_contract_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn rust_body_changes_are_allowed_but_signature_changes_are_blocked() {
        let root = fixture("rust");
        let file = root.join("lib.rs");
        fs::write(
            &file,
            "pub fn value(input: u8) -> u8 { input + 1 }\nfn hidden() {}",
        )
        .unwrap();
        let baseline = ContractLock::capture(&root).unwrap();
        fs::write(
            &file,
            "pub fn value(input: u8) -> u8 { input + 2 }\nfn hidden() { panic!() }",
        )
        .unwrap();
        let body_diff = ContractLock::compare(&baseline, &ContractLock::capture(&root).unwrap());
        assert!(body_diff.compatible(), "{body_diff:?}");
        fs::write(&file, "pub fn value(input: u16) -> u8 { input as u8 }").unwrap();
        let diff = ContractLock::compare(&baseline, &ContractLock::capture(&root).unwrap());
        assert_eq!(diff.changed.len(), 1);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rust_private_modules_do_not_leak_apparent_public_contracts() {
        let root = fixture("rust_visibility");
        let file = root.join("lib.rs");
        fs::write(
            &file,
            "mod private { pub fn internal(input: u8) -> u8 { input } }\n\
             pub mod api { pub fn visible(input: u8) -> u8 { input } }",
        )
        .unwrap();
        let baseline = ContractLock::capture(&root).unwrap();
        fs::write(
            &file,
            "mod private { pub fn internal(input: String) -> String { input } }\n\
             pub mod api { pub fn visible(input: u8) -> u8 { input + 1 } }",
        )
        .unwrap();
        let diff = ContractLock::compare(&baseline, &ContractLock::capture(&root).unwrap());
        assert!(diff.compatible(), "{diff:?}");
        fs::write(
            &file,
            "mod private { pub fn internal(input: String) -> String { input } }\n\
             pub mod api { pub fn visible(input: u16) -> u16 { input } }",
        )
        .unwrap();
        assert!(
            !ContractLock::compare(&baseline, &ContractLock::capture(&root).unwrap()).compatible()
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn typescript_exported_contract_is_locked_and_private_code_is_ignored() {
        let root = fixture("ts");
        let file = root.join("api.ts");
        fs::write(
            &file,
            "export function value(input: number): number { return input + 1 }\nfunction hidden() {}",
        )
        .unwrap();
        let baseline = ContractLock::capture(&root).unwrap();
        fs::write(
            &file,
            "export function value(input: number): number { return input + 2 }\nfunction hidden(x: string) {}",
        )
        .unwrap();
        let body_diff = ContractLock::compare(&baseline, &ContractLock::capture(&root).unwrap());
        assert!(body_diff.compatible(), "{body_diff:?}");
        fs::write(
            &file,
            "export function value(input: string): number { return input.length }",
        )
        .unwrap();
        assert!(
            !ContractLock::compare(&baseline, &ContractLock::capture(&root).unwrap()).compatible()
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rust_and_typescript_reexports_are_locked() {
        let root = fixture("reexports");
        fs::write(
            root.join("lib.rs"),
            "mod internal { pub struct Item; }\npub use internal::Item;",
        )
        .unwrap();
        fs::write(root.join("api.ts"), "export { Item } from './internal';").unwrap();
        let baseline = ContractLock::capture(&root).unwrap();
        fs::write(root.join("lib.rs"), "mod internal { pub struct Item; }").unwrap();
        fs::write(root.join("api.ts"), "export { Other } from './internal';").unwrap();
        let diff = ContractLock::compare(&baseline, &ContractLock::capture(&root).unwrap());
        assert_eq!(diff.removed.len(), 2, "{diff:?}");
        fs::remove_dir_all(root).ok();
    }
}
