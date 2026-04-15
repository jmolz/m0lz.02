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
        // ORM-side: keyed by PHYSICAL table name (post-`@@map`). Holds the
        // ORM model name and the field map (physical-col → ORM field name).
        let mut models: BTreeMap<String, PrismaModel> = BTreeMap::new();
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
                    for parsed in parse_prisma(&content) {
                        let phys = parsed.table_name.clone();
                        model_file.entry(phys.clone()).or_insert(rel.clone());
                        models.insert(phys, parsed);
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
        let mut matched_tables: BTreeSet<String> = BTreeSet::new();

        for (phys_table, model) in &models {
            // Match by PHYSICAL table name, case-insensitive (Postgres
            // default folds unquoted identifiers to lowercase). Without
            // `@@map` this still works because `phys_table == model_name`.
            let ddl_key = ddl_columns
                .keys()
                .find(|t| t.eq_ignore_ascii_case(phys_table))
                .cloned();
            let Some(ddl_key) = ddl_key else {
                // Fail-closed: an ORM model with no matching DDL is schema
                // drift — the archetypal "changed ORM, forgot the migration"
                // case.
                let mut f = SeamFinding::new(format!(
                    "ORM model '{}' (physical table '{}') has no matching migration \
                     table — schema drift: migration is missing or the table was renamed",
                    model.model_name, phys_table
                ));
                if let Some(p) = model_file.get(phys_table).cloned() {
                    f = f.with_file(p);
                }
                findings.push(f);
                continue;
            };
            matched_tables.insert(ddl_key.clone());
            let columns = ddl_columns.get(&ddl_key).cloned().unwrap_or_default();
            // Compare physical column names. `model.fields` is keyed by
            // physical column name (from `@map`, defaulting to ORM field).
            for phys_col in model.fields.keys() {
                if columns.iter().any(|c| c.eq_ignore_ascii_case(phys_col)) {
                    continue;
                }
                let orm_field = model
                    .fields
                    .get(phys_col)
                    .cloned()
                    .unwrap_or_else(|| phys_col.clone());
                let mut f = SeamFinding::new(format!(
                    "ORM model '{}' declares field '{}' (physical column '{}') which is \
                     missing from the migration for table '{}'",
                    model.model_name, orm_field, phys_col, ddl_key
                ));
                if let Some(p) = model_file.get(phys_table).cloned() {
                    f = f.with_file(p);
                }
                findings.push(f);
            }
            for column in &columns {
                if model.fields.keys().any(|c| c.eq_ignore_ascii_case(column)) {
                    continue;
                }
                let mut f = SeamFinding::new(format!(
                    "migration for table '{}' has column '{}' which is missing \
                     from the ORM model '{}'",
                    ddl_key, column, model.model_name
                ));
                if let Some(p) = ddl_file.get(&ddl_key).cloned() {
                    f = f.with_file(p);
                }
                findings.push(f);
            }
        }

        // Symmetric pass: migration tables with no matching ORM model are also
        // drift (migration added without updating the ORM, or model renamed).
        for table in ddl_columns.keys() {
            if matched_tables.iter().any(|m| m.eq_ignore_ascii_case(table)) {
                continue;
            }
            let has_model = models.keys().any(|phys| phys.eq_ignore_ascii_case(table));
            if has_model {
                continue;
            }
            let mut f = SeamFinding::new(format!(
                "migration table '{table}' has no matching ORM model — \
                 schema drift: ORM is missing or the model was renamed"
            ));
            if let Some(p) = ddl_file.get(table).cloned() {
                f = f.with_file(p);
            }
            findings.push(f);
        }

        if findings.is_empty() {
            SeamResult::Passed
        } else {
            SeamResult::Failed(findings)
        }
    }
}

/// Result of parsing a Prisma model. `model_name` is the ORM name used in
/// queries; `table_name` is the physical SQL table name (from `@@map`,
/// defaulting to `model_name`). `fields` maps the physical column name (from
/// `@map`, defaulting to the ORM field name) to the ORM field name. This
/// lets the drift checker compare PHYSICAL names against migration DDL
/// (which knows nothing about Prisma) while still surfacing the ORM-side
/// names in findings for operator clarity.
#[derive(Debug, Default)]
struct PrismaModel {
    model_name: String,
    table_name: String,
    fields: BTreeMap<String, String>,
}

/// Very small Prisma parser — captures `model Foo { field Type }` blocks
/// with `@@map("phys_table")` and `@map("phys_col")` mappings.
fn parse_prisma(content: &str) -> Vec<PrismaModel> {
    let mut out: Vec<PrismaModel> = Vec::new();
    let mut current: Option<PrismaModel> = None;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("model ") {
            // `model Foo {` — name ends at first whitespace or `{`.
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '{')
                .unwrap_or(rest.len());
            let name = rest[..end].trim().to_string();
            if !name.is_empty() {
                if let Some(prev) = current.take() {
                    out.push(prev);
                }
                current = Some(PrismaModel {
                    model_name: name.clone(),
                    table_name: name,
                    fields: BTreeMap::new(),
                });
                continue;
            }
        }
        if line.starts_with('}') {
            if let Some(prev) = current.take() {
                out.push(prev);
            }
            continue;
        }
        if let Some(model) = current.as_mut() {
            // Block-level mapping: `@@map("physical_table")`.
            if let Some(rest) = line.strip_prefix("@@map(") {
                if let Some(name) = parse_quoted_arg(rest) {
                    model.table_name = name;
                }
                continue;
            }
            if line.starts_with("@@") {
                // Other block directives (e.g. `@@index`) are not relevant
                // to drift detection.
                continue;
            }
            // Field line: capture the field name (first token) and any
            // inline `@map("phys_col")` attribute.
            let name = line
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches('?');
            if name.is_empty() || !name.chars().next().is_some_and(|c| c.is_alphabetic()) {
                continue;
            }
            let mut physical_col = name.to_string();
            if let Some(idx) = line.find("@map(") {
                let after = &line[idx + "@map(".len()..];
                if let Some(mapped) = parse_quoted_arg(after) {
                    physical_col = mapped;
                }
            }
            model.fields.insert(physical_col, name.to_string());
        }
    }
    if let Some(prev) = current.take() {
        out.push(prev);
    }
    out
}

/// Parse the first quoted string argument out of `("foo", ...)` or `("foo")`.
/// Tolerates single OR double quotes and surrounding whitespace.
fn parse_quoted_arg(s: &str) -> Option<String> {
    let s = s.trim_start();
    let bytes = s.as_bytes();
    let quote = match bytes.first()? {
        b'"' => b'"',
        b'\'' => b'\'',
        _ => return None,
    };
    let rest = &s[1..];
    let end = rest.bytes().position(|b| b == quote)?;
    Some(rest[..end].to_string())
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
        assert!(
            msg.contains("phone"),
            "finding should mention drifted column: {msg}"
        );
    }

    #[test]
    fn out_of_scope_when_boundary_does_not_touch_data_layer() {
        let b = LayerBoundary::new("frontend", "infrastructure");
        assert!(!SchemaDriftCheck.applies_to(&b));
    }

    #[test]
    fn fails_when_orm_model_has_no_matching_migration() {
        // The archetypal category-9 drift: added a model, forgot to migrate.
        let (dir, rels) = fixture(&[
            (
                "prisma/schema.prisma",
                "model User {\n  id Int @id\n  email String\n}\nmodel Order {\n  id Int @id\n}\n",
            ),
            (
                "migrations/001.sql",
                "CREATE TABLE User (id INT PRIMARY KEY, email TEXT);",
            ),
        ]);
        let boundary = LayerBoundary::new("backend", "database");
        let ctx = ctx(&dir, &boundary, &rels);
        let result = SchemaDriftCheck.run(&ctx);
        assert!(
            result.is_failed(),
            "model with no migration must be Failed, got {result:?}"
        );
        let msgs: Vec<&str> = result
            .findings()
            .iter()
            .map(|f| f.message.as_str())
            .collect();
        assert!(
            msgs.iter()
                .any(|m| m.contains("Order") && m.contains("no matching migration table")),
            "finding should name the orphan model: {msgs:?}"
        );
    }

    #[test]
    fn fails_when_migration_table_has_no_matching_model() {
        // Reverse direction: added a table, forgot the ORM model.
        let (dir, rels) = fixture(&[
            (
                "prisma/schema.prisma",
                "model User {\n  id Int @id\n  email String\n}\n",
            ),
            (
                "migrations/001.sql",
                "CREATE TABLE User (id INT PRIMARY KEY, email TEXT);\n\
                 CREATE TABLE LegacyAudit (id INT PRIMARY KEY, note TEXT);",
            ),
        ]);
        let boundary = LayerBoundary::new("backend", "database");
        let ctx = ctx(&dir, &boundary, &rels);
        let result = SchemaDriftCheck.run(&ctx);
        assert!(
            result.is_failed(),
            "table with no ORM model must be Failed, got {result:?}"
        );
        let msgs: Vec<&str> = result
            .findings()
            .iter()
            .map(|f| f.message.as_str())
            .collect();
        assert!(
            msgs.iter()
                .any(|m| m.contains("LegacyAudit") && m.contains("no matching ORM model")),
            "finding should name the orphan table: {msgs:?}"
        );
    }

    #[test]
    fn parse_prisma_basic() {
        let schema = "model User {\n  id Int @id\n  email String?\n  @@index([email])\n}\n";
        let parsed = parse_prisma(schema);
        let user = parsed
            .iter()
            .find(|m| m.model_name == "User")
            .expect("model parsed");
        assert_eq!(user.table_name, "User");
        assert!(user.fields.contains_key("id"));
        assert!(user.fields.contains_key("email"));
        assert!(!user.fields.contains_key("@@index"));
    }

    #[test]
    fn passes_when_prisma_uses_at_at_map_for_table() {
        // Phase 3 third-round adversarial review fix: previously, an
        // unchanged Prisma schema using `@@map("users")` would be reported
        // as missing-table because the checker compared the ORM model name
        // 'User' against the migration table 'users'. Now the parser
        // captures `@@map` and the checker compares physical names.
        let (dir, rels) = fixture(&[
            (
                "prisma/schema.prisma",
                "model User {\n  id Int @id\n  email String\n  @@map(\"users\")\n}\n",
            ),
            (
                "migrations/001.sql",
                "CREATE TABLE users (id INT PRIMARY KEY, email TEXT);",
            ),
        ]);
        let boundary = LayerBoundary::new("backend", "database");
        let ctx = ctx(&dir, &boundary, &rels);
        let result = SchemaDriftCheck.run(&ctx);
        assert_eq!(
            result,
            SeamResult::Passed,
            "Prisma @@map should match physical table name; got {result:?}"
        );
    }

    #[test]
    fn passes_when_prisma_uses_at_map_for_field() {
        // Field-level `@map("phys_col")` must compare the physical column
        // name against migration columns, not the ORM field name.
        let (dir, rels) = fixture(&[
            (
                "prisma/schema.prisma",
                "model User {\n  id Int @id\n  emailAddress String @map(\"email_address\")\n}\n",
            ),
            (
                "migrations/001.sql",
                "CREATE TABLE User (id INT PRIMARY KEY, email_address TEXT);",
            ),
        ]);
        let boundary = LayerBoundary::new("backend", "database");
        let ctx = ctx(&dir, &boundary, &rels);
        let result = SchemaDriftCheck.run(&ctx);
        assert_eq!(
            result,
            SeamResult::Passed,
            "Prisma @map field should match physical column; got {result:?}"
        );
    }

    #[test]
    fn fails_when_at_at_map_table_is_actually_missing() {
        // `@@map` does not let the schema "lie" — if the physical table
        // really doesn't exist in the migration, drift still fires.
        let (dir, rels) = fixture(&[
            (
                "prisma/schema.prisma",
                "model User {\n  id Int @id\n  @@map(\"missing_table\")\n}\n",
            ),
            (
                "migrations/001.sql",
                "CREATE TABLE users (id INT PRIMARY KEY);",
            ),
        ]);
        let boundary = LayerBoundary::new("backend", "database");
        let ctx = ctx(&dir, &boundary, &rels);
        let result = SchemaDriftCheck.run(&ctx);
        assert!(
            result.is_failed(),
            "@@map pointing at missing table must Failed, got {result:?}"
        );
        let msgs: Vec<&str> = result
            .findings()
            .iter()
            .map(|f| f.message.as_str())
            .collect();
        assert!(
            msgs.iter().any(|m| m.contains("missing_table")),
            "finding should name the missing physical table: {msgs:?}"
        );
    }

    #[test]
    fn parse_sql_handles_if_not_exists_and_pk_clause() {
        let sql =
            "CREATE TABLE IF NOT EXISTS User (\n  id INT,\n  email TEXT,\n  PRIMARY KEY (id)\n);";
        let parsed = parse_create_table(sql);
        let cols = parsed.get("User").unwrap();
        assert!(cols.contains("id"));
        assert!(cols.contains("email"));
        assert!(!cols.contains("PRIMARY"));
    }
}
