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
        let ncl_content = std::fs::read_to_string(ncl_path)
            .with_context(|| format!("Failed to read {}", ncl_path.display()))?;

        // Inject metadata by replacing the import statement
        let processed = self.inject_metadata(&ncl_content);

        // Evaluate the Nickel program
        let json = self.eval_to_json(&processed, ncl_path)?;

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
}
