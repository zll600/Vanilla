use std::ffi::OsString;
use std::path::Path;

use anyhow::{Context as AnyhowContext, Result};
use nickel_lang::Context;

use crate::metadata::Metadata;

use super::schema::OrderPackage;

/// Nickel evaluator with metadata injection
pub struct NickelEvaluator {
    metadata_nickel: String,
}

impl NickelEvaluator {
    /// Create a new evaluator with the given metadata
    pub fn new(metadata: &Metadata) -> Self {
        // Use Nickel record syntax (field = value), not JSON syntax (field: value),
        // because `:` means type annotation in Nickel.
        let metadata_nickel = super::ast_utils::json_to_nickel(&metadata.to_json(), 0);
        Self { metadata_nickel }
    }

    /// Evaluate a order.ncl file and return the parsed package
    pub fn evaluate(&self, ncl_path: &Path) -> Result<OrderPackage> {
        let timing = std::env::var("BLEND_TIMING").is_ok();
        let t_total = std::time::Instant::now();

        let ncl_content = std::fs::read_to_string(ncl_path)
            .with_context(|| format!("Failed to read {}", ncl_path.display()))?;

        // Inject metadata by replacing the import statement
        let processed = self.inject_metadata(&ncl_content);

        // Evaluate the Nickel program
        let t_eval = std::time::Instant::now();
        let json = self.eval_to_json(&processed, ncl_path)?;
        let eval_us = t_eval.elapsed().as_micros();

        // Parse into OrderPackage
        let mut package: OrderPackage = serde_json::from_value(json).with_context(|| {
            format!(
                "Failed to parse order.ncl structure from {}",
                ncl_path.display()
            )
        })?;

        // Resolve defaults for each file entry
        for entry in &mut package.blend.files {
            entry
                .resolve_defaults()
                .with_context(|| format!("Invalid file entry in {}", ncl_path.display()))?;
        }

        if timing {
            eprintln!(
                "[timing] eval {}: total={}us nickel={}us",
                ncl_path.display(),
                t_total.elapsed().as_micros(),
                eval_us
            );
        }

        Ok(package)
    }

    /// Inject metadata into Nickel source by replacing blend://metadata import
    fn inject_metadata(&self, source: &str) -> String {
        // Replace: let metadata = import "blend://metadata" in
        // With: let metadata = { ... actual metadata ... } in
        //
        // Must use Nickel record syntax (field = value) not JSON syntax (field: value)
        // because `:` means type annotation in Nickel, not field assignment.
        let import_pattern = r#"import "blend://metadata""#;
        source.replace(import_pattern, &self.metadata_nickel)
    }

    /// Evaluate Nickel source and return JSON
    fn eval_to_json(&self, source: &str, path: &Path) -> Result<serde_json::Value> {
        let mut ctx = Context::new().with_source_name(path.to_string_lossy().into_owned());

        // Add the parent directory to import paths so relative imports work
        if let Some(parent) = path.parent() {
            let import_paths: Vec<OsString> = vec![parent.as_os_str().to_owned()];
            ctx = ctx.with_added_import_paths(import_paths);
        }

        // Evaluate the Nickel source
        let expr = ctx
            .eval_deep(source)
            .map_err(|e| anyhow::anyhow!("Nickel evaluation error: {e:?}"))?;

        // Export to JSON
        let json_str = ctx
            .expr_to_json(&expr)
            .map_err(|e| anyhow::anyhow!("Failed to export Nickel to JSON: {e:?}"))?;

        let json: serde_json::Value =
            serde_json::from_str(&json_str).with_context(|| "Failed to parse exported JSON")?;

        Ok(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_inject_metadata() {
        let metadata = Metadata {
            os: "darwin".to_string(),
            arch: "aarch64".to_string(),
            hostname: "myhost".to_string(),
            desktop: None,
            home: PathBuf::from("/Users/test"),
            user: "test".to_string(),
        };

        let evaluator = NickelEvaluator::new(&metadata);
        let source = r#"let metadata = import "blend://metadata" in { os = metadata.os }"#;
        let result = evaluator.inject_metadata(source);

        // Should use Nickel syntax (=) not JSON syntax (:)
        assert!(result.contains(r#"os = "darwin""#));
        assert!(!result.contains("blend://metadata"));
    }

    /// Pin Nickel's `&` merge strictness. Several pieces of `surgical_rewrite_with_structure`
    /// and the shadow-walk leaf collection assume that:
    ///   - `&` merging two records with an identical leaf value is fine
    ///     (the merged value equals that value);
    ///   - `&` merging two records with the SAME path bound to DIFFERENT leaf values
    ///     is a hard error (so we never silently lose one operand's value);
    ///   - disjoint fields combine into a single record.
    ///
    /// If Nickel ever loosens this contract, our code that rewrites BOTH operands
    /// to keep them in sync may need to be revisited.
    fn eval(source: &str) -> Result<serde_json::Value> {
        let mut ctx = Context::new();
        let expr = ctx
            .eval_deep(source)
            .map_err(|e| anyhow::anyhow!("eval error: {e:?}"))?;
        let json_str = ctx
            .expr_to_json(&expr)
            .map_err(|e| anyhow::anyhow!("export error: {e:?}"))?;
        Ok(serde_json::from_str(&json_str)?)
    }

    #[test]
    fn test_nickel_merge_rejects_distinct_leaf_values() {
        let err = eval("{a = 1} & {a = 2}").unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("merge") || msg.to_lowercase().contains("conflict"),
            "expected merge conflict error, got: {msg}"
        );
    }

    #[test]
    fn test_nickel_merge_accepts_identical_leaf_values() {
        let v = eval("{a = 1} & {a = 1}").unwrap();
        assert_eq!(v, serde_json::json!({"a": 1}));
    }

    #[test]
    fn test_nickel_merge_combines_disjoint_fields() {
        let v = eval("{a = 1} & {b = 2}").unwrap();
        assert_eq!(v, serde_json::json!({"a": 1, "b": 2}));
    }

    #[test]
    fn test_nickel_merge_recurses_into_subrecords() {
        // Nested records may share field NAMES so long as every leaf agrees.
        let v = eval("{x = {a = 1}} & {x = {b = 2}}").unwrap();
        assert_eq!(v, serde_json::json!({"x": {"a": 1, "b": 2}}));
    }
}
