use std::collections::HashMap;

use anyhow::{Context, Result};
use nickel_lang_parser::{
    ErrorTolerantParser,
    ast::{
        Ast, AstAlloc, Node, StringChunk,
        pattern::{ConstantPatternData, PatternData},
        primop::PrimOp,
        record::FieldDef,
    },
    files::Files,
    grammar,
    lexer::Lexer,
};

use crate::metadata::Metadata;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Result of analyzing a from_config value for rewritability
#[allow(dead_code)]
pub enum RewriteResult {
    /// All fields reach rewritable leaf values
    Rewritable { leaf_spans: Vec<LeafSpan> },
    /// Some fields rewritable, some not
    Partial {
        rewritable: Vec<LeafSpan>,
        non_rewritable: Vec<NonRewritableField>,
    },
    /// Nothing is rewritable (or no from_config found)
    NotRewritable,
}

impl RewriteResult {
    #[cfg(test)]
    pub fn is_rewritable(&self) -> bool {
        matches!(self, RewriteResult::Rewritable { .. })
    }

    pub fn has_any_rewritable(&self) -> bool {
        matches!(
            self,
            RewriteResult::Rewritable { .. } | RewriteResult::Partial { .. }
        )
    }

    /// Get all rewritable leaf spans
    pub fn rewritable_spans(&self) -> &[LeafSpan] {
        match self {
            RewriteResult::Rewritable { leaf_spans } => leaf_spans,
            RewriteResult::Partial { rewritable, .. } => rewritable,
            RewriteResult::NotRewritable => &[],
        }
    }

    /// Get all non-rewritable fields
    pub fn non_rewritable_fields(&self) -> &[NonRewritableField] {
        match self {
            RewriteResult::Partial { non_rewritable, .. } => non_rewritable,
            _ => &[],
        }
    }
}

/// A leaf value's source location, resolved through the shadow walk.
/// Points to the exact byte range of the value to replace — whether it's
/// at the top level or inside a conditional branch.
pub struct LeafSpan {
    /// Field name (key path)
    pub name: String,
    /// Byte offset of the leaf value (from TermPos)
    pub value_start: usize,
    /// End byte offset
    pub value_end: usize,
    /// Trail of conditions followed to reach this value
    pub branch_context: Vec<String>,
}

/// A field whose value cannot be auto-pulled
#[allow(dead_code)]
pub struct NonRewritableField {
    /// Field name
    pub name: String,
    /// Why it can't be rewritten
    pub reason: String,
    /// Conditions followed before reaching the non-rewritable node
    pub branch_context: Vec<String>,
}

// ---------------------------------------------------------------------------
// Shadow walk: context-aware AST analysis
// ---------------------------------------------------------------------------

/// Perform a context-aware walk of the from_config value, following active
/// branches through conditionals using runtime metadata.
///
/// Returns a `RewriteResult` indicating which fields can be surgically rewritten.
fn find_rewritable_value<'ast>(
    ast: &Ast<'ast>,
    metadata: &Metadata,
    context: &mut Vec<String>,
) -> SingleFieldResult {
    match &ast.node {
        // Base cases: plain literals are always rewritable
        Node::Null | Node::Bool(_) | Node::Number(_) | Node::String(_) => {
            if let Some(span) = ast.pos.into_opt() {
                SingleFieldResult::Rewritable {
                    value_start: span.start.into(),
                    value_end: span.end.into(),
                    branch_context: context.clone(),
                }
            } else {
                SingleFieldResult::NotRewritable {
                    reason: "no source position".to_string(),
                    branch_context: context.clone(),
                }
            }
        }

        // String chunks: only if all chunks are literals (no interpolation)
        Node::StringChunks(chunks) => {
            let all_literal = chunks.iter().all(|c| matches!(c, StringChunk::Literal(_)));
            if all_literal {
                if let Some(span) = ast.pos.into_opt() {
                    SingleFieldResult::Rewritable {
                        value_start: span.start.into(),
                        value_end: span.end.into(),
                        branch_context: context.clone(),
                    }
                } else {
                    SingleFieldResult::NotRewritable {
                        reason: "no source position".to_string(),
                        branch_context: context.clone(),
                    }
                }
            } else {
                SingleFieldResult::NotRewritable {
                    reason: "string with interpolation".to_string(),
                    branch_context: context.clone(),
                }
            }
        }

        // Record: recurse per field (handled at the from_config level, not here)
        // When we reach a record as a field value, treat it as a rewritable unit
        Node::Record(_) | Node::Array(_) => {
            if let Some(span) = ast.pos.into_opt() {
                SingleFieldResult::Rewritable {
                    value_start: span.start.into(),
                    value_end: span.end.into(),
                    branch_context: context.clone(),
                }
            } else {
                SingleFieldResult::NotRewritable {
                    reason: "no source position".to_string(),
                    branch_context: context.clone(),
                }
            }
        }

        // Match expression applied to an argument: metadata.field |> match { ... }
        Node::App { head, args } if matches!(head.node, Node::Match(_)) => {
            if let Node::Match(m) = &head.node {
                // Try to resolve the argument against metadata
                if let Some(arg) = args.first() {
                    if let Some(resolved) = try_resolve_metadata_field(&arg.node, metadata) {
                        // Find the matching branch
                        for branch in m.branches {
                            if match_pattern(&branch.pattern.data, &resolved) {
                                let condition_desc = format_match_context(&arg.node, &resolved);
                                context.push(condition_desc);
                                return find_rewritable_value(&branch.body, metadata, context);
                            }
                        }
                        SingleFieldResult::NotRewritable {
                            reason: format!("no match branch for value \"{}\"", resolved),
                            branch_context: context.clone(),
                        }
                    } else {
                        SingleFieldResult::NotRewritable {
                            reason: "cannot resolve match argument against metadata".to_string(),
                            branch_context: context.clone(),
                        }
                    }
                } else {
                    SingleFieldResult::NotRewritable {
                        reason: "match applied without argument".to_string(),
                        branch_context: context.clone(),
                    }
                }
            } else {
                unreachable!()
            }
        }

        // If-then-else
        Node::IfThenElse {
            cond,
            then_branch,
            else_branch,
        } => {
            if let Some(result) = try_evaluate_condition(&cond.node, metadata) {
                let branch_name = if result { "then" } else { "else" };
                let condition_desc = format!("if condition → {}", branch_name);
                context.push(condition_desc);
                let active = if result { then_branch } else { else_branch };
                find_rewritable_value(active, metadata, context)
            } else {
                SingleFieldResult::NotRewritable {
                    reason: "cannot evaluate if condition".to_string(),
                    branch_context: context.clone(),
                }
            }
        }

        // Everything else: not rewritable
        _ => SingleFieldResult::NotRewritable {
            reason: "unsupported expression type".to_string(),
            branch_context: context.clone(),
        },
    }
}

/// Result for a single field's shadow walk
enum SingleFieldResult {
    Rewritable {
        value_start: usize,
        value_end: usize,
        branch_context: Vec<String>,
    },
    NotRewritable {
        reason: String,
        branch_context: Vec<String>,
    },
}

/// Try to resolve a metadata field access from an AST node.
///
/// Handles the pattern: `PrimOpApp { op: RecordStatAccess("field"), args: [Var("metadata")] }`
fn try_resolve_metadata_field(node: &Node, metadata: &Metadata) -> Option<String> {
    if let Node::PrimOpApp { op, args } = node
        && let PrimOp::RecordStatAccess(field_ident) = op
    {
        // Check that the record being accessed is `metadata`
        if let Some(arg) = args.first()
            && let Node::Var(var_ident) = &arg.node
            && var_ident.label() == "metadata"
        {
            let field_name = field_ident.label();
            return match field_name {
                "os" => Some(metadata.os.clone()),
                "arch" => Some(metadata.arch.clone()),
                "hostname" => Some(metadata.hostname.clone()),
                "desktop" => metadata.desktop.clone(),
                "user" => Some(metadata.user.clone()),
                "home" => Some(metadata.home.to_string_lossy().to_string()),
                _ => None,
            };
        }
    }
    None
}

/// Try to evaluate a simple boolean condition against metadata.
///
/// Handles: `PrimOpApp { op: Eq, args: [metadata_access, String("value")] }`
fn try_evaluate_condition(node: &Node, metadata: &Metadata) -> Option<bool> {
    if let Node::PrimOpApp { op, args } = node
        && let PrimOp::Eq = op
        && args.len() == 2
    {
        // Try both orderings: metadata.field == "value" and "value" == metadata.field
        if let Some(meta_val) = try_resolve_metadata_field(&args[0].node, metadata)
            && let Some(lit_val) = try_extract_string_literal(&args[1].node)
        {
            return Some(meta_val == lit_val);
        }
        if let Some(meta_val) = try_resolve_metadata_field(&args[1].node, metadata)
            && let Some(lit_val) = try_extract_string_literal(&args[0].node)
        {
            return Some(meta_val == lit_val);
        }
    }
    // Could extend to handle BoolAnd, BoolOr, etc. in the future
    None
}

/// Extract a string literal from an AST node
fn try_extract_string_literal<'a>(node: &'a Node) -> Option<&'a str> {
    match node {
        Node::String(s) => Some(s),
        Node::StringChunks(chunks) if chunks.len() == 1 => {
            if let StringChunk::Literal(s) = &chunks[0] {
                Some(s.as_str())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Check if a Nickel pattern matches a string value
fn match_pattern(pattern: &PatternData, value: &str) -> bool {
    match pattern {
        PatternData::Wildcard => true,
        PatternData::Any(_) => true,
        PatternData::Constant(cp) => match &cp.data {
            ConstantPatternData::String(s) => *s == value,
            _ => false,
        },
        _ => false,
    }
}

/// Format a human-readable description of a match condition
fn format_match_context(arg_node: &Node, resolved_value: &str) -> String {
    if let Node::PrimOpApp { op, .. } = arg_node
        && let PrimOp::RecordStatAccess(field) = op
    {
        return format!("{} == \"{}\"", field.label(), resolved_value);
    }
    format!("matched \"{}\"", resolved_value)
}

// ---------------------------------------------------------------------------
// Public API: locate and analyze from_config
// ---------------------------------------------------------------------------

/// Parse a .ncl file and analyze the from_config field for a specific file entry.
///
/// Uses a context-aware shadow walk with runtime metadata to determine which
/// field values can be surgically rewritten (including values inside conditional branches).
pub fn locate_from_config(
    source: &str,
    file_entry_index: usize,
    metadata: &Metadata,
) -> Result<RewriteResult> {
    let alloc = AstAlloc::new();
    let mut files = Files::empty();
    let file_id = files.add("order.ncl", source);
    let lexer = Lexer::new(source);

    let parser = grammar::TermParser::new();
    let ast = parser
        .parse_strict(&alloc, file_id, lexer)
        .map_err(|e| anyhow::anyhow!("Failed to parse .ncl file: {:?}", e))?;

    // Navigate to from_config
    let root_record = unwrap_to_record(&ast)?;

    let blend_field = find_field(root_record.field_defs, "blend")
        .context("No 'blend' field found in order.ncl")?;
    let blend_value = blend_field
        .value
        .as_ref()
        .context("'blend' field has no value")?;
    let blend_record = match &blend_value.node {
        Node::Record(r) => *r,
        _ => anyhow::bail!("'blend' field is not a record"),
    };

    let files_field =
        find_field(blend_record.field_defs, "files").context("No 'files' field found in blend")?;
    let files_value = files_field
        .value
        .as_ref()
        .context("'files' field has no value")?;
    let files_array = match &files_value.node {
        Node::Array(arr) => *arr,
        _ => anyhow::bail!("'files' is not an array"),
    };

    let entry_ast = files_array
        .get(file_entry_index)
        .context("file_entry_index out of bounds")?;
    let entry_record = match &entry_ast.node {
        Node::Record(r) => *r,
        _ => anyhow::bail!("file entry is not a record"),
    };

    let from_config_field = match find_field(entry_record.field_defs, "from_config") {
        Some(f) => f,
        None => return Ok(RewriteResult::NotRewritable),
    };
    let from_config_value = match &from_config_field.value {
        Some(v) => v,
        None => return Ok(RewriteResult::NotRewritable),
    };

    // If from_config is a record, analyze each field individually
    if let Node::Record(config_record) = &from_config_value.node {
        let mut rewritable = Vec::new();
        let mut non_rewritable = Vec::new();

        for fd in config_record.field_defs {
            let name = match fd.path_as_ident() {
                Some(id) => id.label().to_string(),
                None => continue,
            };

            let value = match &fd.value {
                Some(v) => v,
                None => continue,
            };

            let mut context = Vec::new();
            match find_rewritable_value(value, metadata, &mut context) {
                SingleFieldResult::Rewritable {
                    value_start,
                    value_end,
                    branch_context,
                } => {
                    rewritable.push(LeafSpan {
                        name,
                        value_start,
                        value_end,
                        branch_context,
                    });
                }
                SingleFieldResult::NotRewritable {
                    reason,
                    branch_context,
                } => {
                    non_rewritable.push(NonRewritableField {
                        name,
                        reason,
                        branch_context,
                    });
                }
            }
        }

        if non_rewritable.is_empty() && !rewritable.is_empty() {
            Ok(RewriteResult::Rewritable {
                leaf_spans: rewritable,
            })
        } else if !rewritable.is_empty() {
            Ok(RewriteResult::Partial {
                rewritable,
                non_rewritable,
            })
        } else {
            Ok(RewriteResult::NotRewritable)
        }
    } else {
        // from_config is not a record (unusual) — try to analyze as a single value
        let mut context = Vec::new();
        match find_rewritable_value(from_config_value, metadata, &mut context) {
            SingleFieldResult::Rewritable {
                value_start,
                value_end,
                branch_context,
            } => Ok(RewriteResult::Rewritable {
                leaf_spans: vec![LeafSpan {
                    name: String::new(),
                    value_start,
                    value_end,
                    branch_context,
                }],
            }),
            SingleFieldResult::NotRewritable { .. } => Ok(RewriteResult::NotRewritable),
        }
    }
}

// ---------------------------------------------------------------------------
// AST navigation helpers
// ---------------------------------------------------------------------------

/// Unwrap let-bindings, annotations, etc. to find the root record
fn unwrap_to_record<'ast>(
    ast: &'ast Ast<'ast>,
) -> Result<&'ast nickel_lang_parser::ast::record::Record<'ast>> {
    match &ast.node {
        Node::Record(r) => Ok(r),
        Node::Let { body, .. } => unwrap_to_record(body),
        Node::Annotated { inner, .. } => unwrap_to_record(inner),
        other => anyhow::bail!(
            "Expected record at top level, found {:?}",
            std::mem::discriminant(other)
        ),
    }
}

/// Find a field definition by name in a slice of field defs
fn find_field<'a, 'ast>(
    field_defs: &'a [FieldDef<'ast>],
    name: &str,
) -> Option<&'a FieldDef<'ast>> {
    field_defs.iter().find(|fd| {
        fd.path_as_ident()
            .map(|id| id.label() == name)
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// Surgical rewrite using LeafSpans
// ---------------------------------------------------------------------------

/// Perform a surgical rewrite of from_config values using shadow-walk LeafSpans.
///
/// Only rewrites fields that have changed between current and deployed JSON.
/// Uses the exact byte spans from the shadow walk (which may point inside
/// conditional branches).
pub fn surgical_rewrite(
    source: &str,
    leaf_spans: &[LeafSpan],
    current_json: &serde_json::Value,
    deployed_json: &serde_json::Value,
    base_indent: usize,
) -> Result<String> {
    let current_map = current_json
        .as_object()
        .context("current from_config value is not an object")?;
    let deployed_map = deployed_json
        .as_object()
        .context("deployed value is not an object")?;

    // Build name → LeafSpan lookup
    let span_map: HashMap<&str, &LeafSpan> =
        leaf_spans.iter().map(|ls| (ls.name.as_str(), ls)).collect();

    // Collect byte-range edits for changed values
    let mut edits: Vec<(usize, usize, String)> = Vec::new();

    for (key, deployed_val) in deployed_map {
        if let Some(current_val) = current_map.get(key)
            && current_val != deployed_val
            && let Some(leaf) = span_map.get(key.as_str())
        {
            let new_value = json_to_nickel(deployed_val, base_indent + 1);
            edits.push((leaf.value_start, leaf.value_end, new_value));
        }
        // Additions (key in deployed but not current) are harder with shadow walk
        // since we'd need to insert inside potentially conditional structures.
        // For now, additions are only supported for fully-rewritable from_config blocks.
    }

    // Sort edits in reverse order and apply
    edits.sort_by(|a, b| b.0.cmp(&a.0));

    let mut result = source.to_string();
    for (start, end, replacement) in &edits {
        result.replace_range(*start..*end, replacement);
    }

    Ok(result)
}

/// Determine the indentation level of a from_config block by looking at the source.
pub fn detect_indent_level(source: &str, offset: usize) -> usize {
    let before = &source[..offset];
    let line_start = before.rfind('\n').map(|p| p + 1).unwrap_or(0);
    let line_prefix = &source[line_start..offset];
    let spaces = line_prefix.len() - line_prefix.trim_start().len();
    spaces / 2
}

// ---------------------------------------------------------------------------
// JSON ↔ Nickel serialization
// ---------------------------------------------------------------------------

/// Serialize a serde_json::Value to Nickel data literal syntax.
pub fn json_to_nickel(value: &serde_json::Value, indent: usize) -> String {
    let indent_str = "  ".repeat(indent);
    let inner_indent = "  ".repeat(indent + 1);

    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => format!("\"{}\"", escape_nickel_string(s)),
        serde_json::Value::Array(arr) => {
            if arr.is_empty() {
                return "[]".to_string();
            }
            if arr.len() == 1 && is_simple_value(&arr[0]) {
                return format!("[{}]", json_to_nickel(&arr[0], 0));
            }
            let mut out = "[\n".to_string();
            for elem in arr.iter() {
                out.push_str(&inner_indent);
                out.push_str(&json_to_nickel(elem, indent + 1));
                out.push(',');
                out.push('\n');
            }
            out.push_str(&indent_str);
            out.push(']');
            out
        }
        serde_json::Value::Object(map) => {
            if map.is_empty() {
                return "{}".to_string();
            }
            if map.len() <= 2 && map.values().all(is_simple_value) {
                let pairs: Vec<String> = map
                    .iter()
                    .map(|(k, v)| format!("{} = {}", format_nickel_key(k), json_to_nickel(v, 0)))
                    .collect();
                return format!("{{ {} }}", pairs.join(", "));
            }
            let mut out = "{\n".to_string();
            for (k, v) in map {
                out.push_str(&inner_indent);
                out.push_str(&format_nickel_key(k));
                out.push_str(" = ");
                out.push_str(&json_to_nickel(v, indent + 1));
                out.push_str(",\n");
            }
            out.push_str(&indent_str);
            out.push('}');
            out
        }
    }
}

fn is_simple_value(value: &serde_json::Value) -> bool {
    matches!(
        value,
        serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_)
    )
}

pub fn format_nickel_key(key: &str) -> String {
    if is_valid_nickel_ident(key) {
        key.to_string()
    } else {
        format!("\"{}\"", escape_nickel_string(key))
    }
}

fn is_valid_nickel_ident(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '\'')
}

fn escape_nickel_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if !c.is_ascii() => {
                out.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_metadata(os: &str) -> Metadata {
        Metadata {
            os: os.to_string(),
            arch: "aarch64".to_string(),
            hostname: "testhost".to_string(),
            desktop: None,
            home: PathBuf::from("/home/test"),
            user: "test".to_string(),
        }
    }

    #[test]
    fn test_locate_plain_data() {
        let source = r#"{
  blend = {
    files = [
      {
        name = "test.toml",
        from_config = {
          key = "value",
          number = 42,
        },
      },
    ],
  },
}"#;
        let meta = test_metadata("darwin");
        let result = locate_from_config(source, 0, &meta).unwrap();
        assert!(
            result.is_rewritable(),
            "Plain data should be fully rewritable"
        );

        let spans = result.rewritable_spans();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].name, "key");
        assert_eq!(spans[1].name, "number");
        // Plain data should have empty branch context
        assert!(spans[0].branch_context.is_empty());
    }

    #[test]
    fn test_locate_match_expression() {
        let source = r#"let metadata = { os = "darwin", arch = "aarch64", hostname = "test", user = "test", home = "/home/test" } in {
  blend = {
    files = [
      {
        name = "test.toml",
        from_config = {
          font_size = metadata.os |> match {
            "darwin" => 14,
            _ => 12,
          },
          name = "hello",
        },
      },
    ],
  },
}"#;
        let meta = test_metadata("darwin");
        let result = locate_from_config(source, 0, &meta).unwrap();
        assert!(
            result.is_rewritable(),
            "Match resolving to literal should be rewritable"
        );

        let spans = result.rewritable_spans();
        assert_eq!(spans.len(), 2);

        // font_size resolved through match
        let font_span = &spans[0];
        assert_eq!(font_span.name, "font_size");
        assert!(
            !font_span.branch_context.is_empty(),
            "Should have branch context"
        );
        assert!(font_span.branch_context[0].contains("darwin"));

        // Check the span points to "14"
        let value_text = &source[font_span.value_start..font_span.value_end];
        assert_eq!(value_text, "14");
    }

    #[test]
    fn test_locate_match_wildcard_branch() {
        let source = r#"let metadata = { os = "linux", arch = "x86_64", hostname = "test", user = "test", home = "/home/test" } in {
  blend = {
    files = [
      {
        name = "test.toml",
        from_config = {
          font_size = metadata.os |> match {
            "darwin" => 14,
            _ => 12,
          },
        },
      },
    ],
  },
}"#;
        let meta = test_metadata("linux");
        let result = locate_from_config(source, 0, &meta).unwrap();
        assert!(result.is_rewritable());

        let spans = result.rewritable_spans();
        let font_span = &spans[0];
        let value_text = &source[font_span.value_start..font_span.value_end];
        assert_eq!(value_text, "12");
    }

    #[test]
    fn test_locate_with_variable_ref_not_rewritable() {
        let source = r#"let x = 42 in {
  blend = {
    files = [
      {
        name = "test.toml",
        from_config = {
          key = x,
        },
      },
    ],
  },
}"#;
        let meta = test_metadata("darwin");
        let result = locate_from_config(source, 0, &meta).unwrap();
        assert!(!result.has_any_rewritable());
    }

    #[test]
    fn test_locate_no_from_config() {
        let source = r#"{
  blend = {
    files = [
      {
        name = "test.txt",
        from_file = "test.txt",
      },
    ],
  },
}"#;
        let meta = test_metadata("darwin");
        let result = locate_from_config(source, 0, &meta).unwrap();
        assert!(matches!(result, RewriteResult::NotRewritable));
    }

    #[test]
    fn test_locate_partial_rewritability() {
        let source = r#"let base = 10 in
let metadata = { os = "darwin", arch = "aarch64", hostname = "test", user = "test", home = "/home/test" } in {
  blend = {
    files = [
      {
        name = "test.toml",
        from_config = {
          plain_key = "value",
          computed_key = base,
        },
      },
    ],
  },
}"#;
        let meta = test_metadata("darwin");
        let result = locate_from_config(source, 0, &meta).unwrap();

        match &result {
            RewriteResult::Partial {
                rewritable,
                non_rewritable,
            } => {
                assert_eq!(rewritable.len(), 1);
                assert_eq!(rewritable[0].name, "plain_key");
                assert_eq!(non_rewritable.len(), 1);
                assert_eq!(non_rewritable[0].name, "computed_key");
            }
            other => panic!(
                "Expected Partial, got {:?}",
                matches!(other, RewriteResult::NotRewritable)
            ),
        }
    }

    #[test]
    fn test_locate_if_then_else() {
        let source = r#"let metadata = { os = "darwin", arch = "aarch64", hostname = "test", user = "test", home = "/home/test" } in {
  blend = {
    files = [
      {
        name = "test.toml",
        from_config = {
          font_size = if metadata.os == "darwin" then 14 else 12,
        },
      },
    ],
  },
}"#;
        let meta = test_metadata("darwin");
        let result = locate_from_config(source, 0, &meta).unwrap();
        assert!(
            result.has_any_rewritable(),
            "if-then-else resolving to literal should be rewritable"
        );

        let spans = result.rewritable_spans();
        let value_text = &source[spans[0].value_start..spans[0].value_end];
        assert_eq!(value_text, "14");
        assert!(!spans[0].branch_context.is_empty());
    }

    #[test]
    fn test_locate_if_then_else_false_branch() {
        let source = r#"let metadata = { os = "linux", arch = "x86_64", hostname = "test", user = "test", home = "/home/test" } in {
  blend = {
    files = [
      {
        name = "test.toml",
        from_config = {
          font_size = if metadata.os == "darwin" then 14 else 12,
        },
      },
    ],
  },
}"#;
        let meta = test_metadata("linux");
        let result = locate_from_config(source, 0, &meta).unwrap();
        assert!(result.has_any_rewritable());

        let spans = result.rewritable_spans();
        let value_text = &source[spans[0].value_start..spans[0].value_end];
        assert_eq!(value_text, "12");
    }

    #[test]
    fn test_locate_match_three_branches() {
        let source = r#"let metadata = { os = "darwin", arch = "aarch64", hostname = "test", user = "test", home = "/home/test" } in {
  blend = {
    files = [
      {
        name = "test.toml",
        from_config = {
          shell = metadata.os |> match {
            "darwin" => "/bin/zsh",
            "linux" => "/bin/bash",
            _ => "/bin/sh",
          },
        },
      },
    ],
  },
}"#;
        // Test darwin → "/bin/zsh"
        let meta_darwin = test_metadata("darwin");
        let result = locate_from_config(source, 0, &meta_darwin).unwrap();
        let spans = result.rewritable_spans();
        let value_text = &source[spans[0].value_start..spans[0].value_end];
        assert_eq!(value_text, "\"/bin/zsh\"");

        // Test linux → "/bin/bash"
        let meta_linux = test_metadata("linux");
        let result = locate_from_config(source, 0, &meta_linux).unwrap();
        let spans = result.rewritable_spans();
        let value_text = &source[spans[0].value_start..spans[0].value_end];
        assert_eq!(value_text, "\"/bin/bash\"");

        // Test windows → wildcard → "/bin/sh"
        let meta_win = test_metadata("windows");
        let result = locate_from_config(source, 0, &meta_win).unwrap();
        let spans = result.rewritable_spans();
        let value_text = &source[spans[0].value_start..spans[0].value_end];
        assert_eq!(value_text, "\"/bin/sh\"");
    }

    #[test]
    fn test_locate_metadata_arch() {
        let source = r#"let metadata = { os = "darwin", arch = "aarch64", hostname = "test", user = "test", home = "/home/test" } in {
  blend = {
    files = [
      {
        name = "test.toml",
        from_config = {
          prefix = metadata.arch |> match {
            "aarch64" => "/opt/homebrew",
            _ => "/usr/local",
          },
        },
      },
    ],
  },
}"#;
        let meta = test_metadata("darwin");
        let result = locate_from_config(source, 0, &meta).unwrap();
        let spans = result.rewritable_spans();
        let value_text = &source[spans[0].value_start..spans[0].value_end];
        assert_eq!(value_text, "\"/opt/homebrew\"");
    }

    #[test]
    fn test_locate_nested_record_value() {
        let source = r#"{
  blend = {
    files = [
      {
        name = "test.toml",
        from_config = {
          section = {
            inner_key = "inner_val",
            inner_num = 99,
          },
        },
      },
    ],
  },
}"#;
        let meta = test_metadata("darwin");
        let result = locate_from_config(source, 0, &meta).unwrap();
        assert!(result.is_rewritable());
        let spans = result.rewritable_spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "section");
        // The span should cover the entire nested record
        let value_text = &source[spans[0].value_start..spans[0].value_end];
        assert!(value_text.contains("inner_key"));
        assert!(value_text.contains("99"));
    }

    #[test]
    fn test_surgical_rewrite_with_leaf_spans() {
        use serde_json::json;

        let source = "header from_config = { key = \"old_value\", num = 10 } trailer";

        let leaf_spans = vec![
            LeafSpan {
                name: "key".to_string(),
                value_start: 29, // start of "old_value"
                value_end: 40,   // end of "old_value"
                branch_context: vec![],
            },
            LeafSpan {
                name: "num".to_string(),
                value_start: 48, // start of 10
                value_end: 50,   // end of 10
                branch_context: vec![],
            },
        ];

        let current: serde_json::Value = json!({"key": "old_value", "num": 10});
        let deployed: serde_json::Value = json!({"key": "new_value", "num": 20});

        let result = surgical_rewrite(source, &leaf_spans, &current, &deployed, 0).unwrap();
        assert!(result.contains("\"new_value\""));
        assert!(result.contains("20"));
        assert!(result.contains("header"));
        assert!(result.contains("trailer"));
    }

    #[test]
    fn test_json_to_nickel_simple() {
        use serde_json::json;
        assert_eq!(json_to_nickel(&json!(null), 0), "null");
        assert_eq!(json_to_nickel(&json!(true), 0), "true");
        assert_eq!(json_to_nickel(&json!(42), 0), "42");
        assert_eq!(json_to_nickel(&json!("hello"), 0), "\"hello\"");
    }

    #[test]
    fn test_json_to_nickel_record() {
        use serde_json::json;
        let val = json!({"key": "value"});
        assert_eq!(json_to_nickel(&val, 0), "{ key = \"value\" }");
    }

    #[test]
    fn test_json_to_nickel_unicode_escape() {
        use serde_json::json;
        let val = json!("\u{e76f} ");
        assert_eq!(json_to_nickel(&val, 0), "\"\\u{e76f} \"");
    }

    #[test]
    fn test_escape_nickel_string() {
        assert_eq!(escape_nickel_string("hello"), "hello");
        assert_eq!(escape_nickel_string("he\"llo"), "he\\\"llo");
        assert_eq!(escape_nickel_string("\u{e76f}"), "\\u{e76f}");
    }

    #[test]
    fn test_format_nickel_key() {
        assert_eq!(format_nickel_key("simple"), "simple");
        assert_eq!(format_nickel_key("$schema"), "\"$schema\"");
    }

    #[test]
    fn test_detect_indent_level() {
        assert_eq!(detect_indent_level("    from_config = {\n", 18), 2);
        assert_eq!(detect_indent_level("top = {\n", 6), 0);
    }
}
