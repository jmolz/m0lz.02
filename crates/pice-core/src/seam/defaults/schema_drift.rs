//! Category 9 — Serialization / schema drift.
//!
//! Detects ORM model fields that don't appear in the corresponding migration
//! DDL (or vice versa). Currently supports Prisma schemas and simple
//! `CREATE TABLE` DDL; plugin crates can provide richer parsers.

use crate::seam::types::{LayerBoundary, SeamCheck, SeamContext, SeamFinding, SeamResult};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub struct SchemaDriftCheck;

impl SeamCheck for SchemaDriftCheck {
    fn id(&self) -> &str {
        "schema_drift"
    }
    fn category(&self) -> u8 {
        9
    }
    fn applies_to(&self, boundary: &LayerBoundary) -> bool {
        boundary.touches("database") || boundary.touches("backend") || boundary.touches("api")
    }
    fn run(&self, ctx: &SeamContext<'_>) -> SeamResult {
        let mut model_fields: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut model_file: BTreeMap<String, PathBuf> = BTreeMap::new();
        let mut ddl_columns: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut ddl_file: BTreeMap<String, PathBuf> = BTreeMap::new();

        for rel in ctx.boundary_files {
            let full = ctx.repo_root.join(rel);
            let Ok(content) = std::fs::read_to_string(&full) else {
                continue;
            };
            let ext = rel
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            match ext.as_str() {
                "prisma" => {
                    for (model, fields) in parse_prisma(&content) {
                        model_fields
                            .entry(model.clone())
                            .or_default()
                            .extend(fields);
                        model_file.entry(model).or_insert(rel.clone());
                    }
                }
                "sql" => {
                    for (table, columns) in parse_create_table(&content) {
                        ddl_columns
                            .entry(table.clone())
                            .or_default()
                            .extend(columns);
                        ddl_file.entry(table).or_insert(rel.clone());
                    }
                }
                _ => {}
            }
        }

        let mut findings: Vec<SeamFinding> = Vec::new();

        for (model, fields) in &model_fields {
            // Match model → table by case-insensitive name (Prisma default).
            let table_key = ddl_columns
                .keys()
                .find(|t| t.eq_ignore_ascii_case(model))
                .cloned();
            let Some(table) = table_key else {
                continue;
            };
            let columns = ddl_columns.get(&table).cloned().unwrap_or_default();
            for field in fields.difference(&columns) {
                let mut f = SeamFinding::new(format!(
                    "ORM model '{model}' declares field '{field}' which is missing from the \
                     migration for table '{table}'"
                ));
                if let Some(p) = model_file.get(model).cloned() {
                    f = f.with_file(p);
                }
                findings.push(f);
            }
            for column in columns.difference(fields) {
                let mut f = SeamFinding::new(format!(
                    "migration for table '{table}' has column '{column}' which is missing \
                     from the ORM model '{model}'"
                ));
                if let Some(p) = ddl_file.get(&table).cloned() {
                    f = f.with_file(p);
                }
                findings.push(f);
            }
        }

        if findings.is_empty() {
            SeamResult::Passed
        } else {
            SeamResult::Failed(findings)
        }
    }
}

/// Very small Prisma parser — captures `model Foo { field Type }` blocks.
fn parse_prisma(content: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("model ") {
            // `model Foo {` — name ends at first whitespace or `{`.
            let end = rest.find(|c: char| c.is_whitespace() || c == '{').unwrap_or(rest.len());
            let name = rest[..end].trim().to_string();
            if !name.is_empty() {
                current = Some(name);
                continue;
            }
        }
        if line.starts_with('}') {
            current = None;
            continue;
        }
        if let Some(model) = current.as_ref() {
            // First whitespace-delimited token is the field name. Skip
            // blank lines and the `@@...` block-level directives.
            if line.starts_with("@@") || line.starts_with('@') {
                continue;
            }
            let name = line
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches('?');
            if !name.is_empty() && name.chars().next().is_some_and(|c| c.is_alphabetic()) {
                out.entry(model.clone()).or_default().insert(name.to_string());
            }
        }
    }
    out
}

/// Parse `CREATE TABLE name ( col type, ... );` blocks. Handles quoted
/// identifiers and `IF NOT EXISTS`. Not a full SQL parser — only used to
/// recover column names for drift comparison.
fn parse_create_table(content: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let upper = content.to_ascii_uppercase();
    let bytes = content.as_bytes();
    let mut pos = 0usize;

    while let Some(offset) = upper[pos..].find("CREATE TABLE") {
        let start = pos + offset + "CREATE TABLE".len();
        // Optional "IF NOT EXISTS".
        let mut cursor = start;
        let rest = &content[cursor..];
        let rest_trim_start = rest.len() - rest.trim_start().len();
        cursor += rest_trim_start;
        if content[cursor..]
            .to_ascii_uppercase()
            .starts_with("IF NOT EXISTS")
        {
            cursor += "IF NOT EXISTS".len();
        }
        // Table name.
        let head = &content[cursor..];
        let head_trim = head.len() - head.trim_start().len();
        cursor += head_trim;
        let name_slice = &content[cursor..];
        let name_end = name_slice
            .find(|c: char| c == '(' || c.is_whitespace())
            .unwrap_or(name_slice.len());
        let raw_name = name_slice[..name_end].trim_matches(['"', '`', '[', ']']);
        let table = raw_name.to_string();
        cursor += name_end;

        // Column list — between the next '(' and its matching ')'.
        let Some(open_rel) = content[cursor..].find('(') else {
            break;
        };
        let open = cursor + open_rel;
        let close = match_paren(bytes, open);
        let Some(close) = close else { break };
        let inner = &content[open + 1..close];
        let mut cols: BTreeSet<String> = BTreeSet::new();
        for part in split_top_level_commas(inner) {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                continue;
            }
            let up = trimmed.to_ascii_uppercase();
            if up.starts_with("PRIMARY KEY")
                || up.starts_with("FOREIGN KEY")
                || up.starts_with("CONSTRAINT")
                || up.starts_with("UNIQUE")
                || up.starts_with("CHECK")
            {
                continue;
            }
            let first = trimmed
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches(['"', '`', '[', ']']);
            if !first.is_empty() {
                cols.insert(first.to_string());
            }
        }
        if !table.is_empty() {
            out.entry(table).or_default().extend(cols);
        }
        pos = close + 1;
    }
    out
}

fn match_paren(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    if start <= s.len() {
        out.push(&s[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(
        dir: &'a tempfile::TempDir,
        boundary: &'a LayerBoundary,
        rels: &'a [PathBuf],
    ) -> SeamContext<'a> {
        SeamContext {
            boundary,
            filtered_diff: "",
            repo_root: dir.path(),
            boundary_files: rels,
            args: None,
        }
    }

    fn fixture(files: &[(&str, &str)]) -> (tempfile::TempDir, Vec<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();
        let mut rels = Vec::new();
        for (rel, content) in files {
            let full = dir.path().join(rel);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&full, content).unwrap();
            rels.push(PathBuf::from(rel));
        }
        (dir, rels)
    }

    #[test]
    fn passes_when_model_matches_migration() {
        let (dir, rels) = fixture(&[
            (
                "prisma/schema.prisma",
                "model User {\n  id Int @id\n  email String\n}\n",
            ),
            (
                "migrations/001.sql",
                "CREATE TABLE User (id INT PRIMARY KEY, email TEXT);",
            ),
        ]);
        let boundary = LayerBoundary::new("backend", "database");
        let ctx = ctx(&dir, &boundary, &rels);
        assert_eq!(SchemaDriftCheck.run(&ctx), SeamResult::Passed);
    }

    #[test]
    fn fails_when_model_field_missing_from_migration() {
        let (dir, rels) = fixture(&[
            (
                "prisma/schema.prisma",
                "model User {\n  id Int @id\n  email String\n  phone String\n}\n",
            ),
            (
                "migrations/001.sql",
                "CREATE TABLE User (id INT PRIMARY KEY, email TEXT);",
            ),
        ]);
        let boundary = LayerBoundary::new("backend", "database");
        let ctx = ctx(&dir, &boundary, &rels);
        let result = SchemaDriftCheck.run(&ctx);
        assert!(result.is_failed(), "expected drift finding, got {result:?}");
        let msg = &result.findings()[0].message;
        assert!(msg.contains("phone"), "finding should mention drifted column: {msg}");
    }

    #[test]
    fn out_of_scope_when_boundary_does_not_touch_data_layer() {
        let b = LayerBoundary::new("frontend", "infrastructure");
        assert!(!SchemaDriftCheck.applies_to(&b));
    }

    #[test]
    fn parse_prisma_basic() {
        let schema = "model User {\n  id Int @id\n  email String?\n  @@index([email])\n}\n";
        let parsed = parse_prisma(schema);
        let fields = parsed.get("User").unwrap();
        assert!(fields.contains("id"));
        assert!(fields.contains("email"));
        assert!(!fields.contains("@@index"));
    }

    #[test]
    fn parse_sql_handles_if_not_exists_and_pk_clause() {
        let sql = "CREATE TABLE IF NOT EXISTS User (\n  id INT,\n  email TEXT,\n  PRIMARY KEY (id)\n);";
        let parsed = parse_create_table(sql);
        let cols = parsed.get("User").unwrap();
        assert!(cols.contains("id"));
        assert!(cols.contains("email"));
        assert!(!cols.contains("PRIMARY"));
    }
}
