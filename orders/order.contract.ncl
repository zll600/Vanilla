# Contract for order.ncl files
# Defines the expected structure for blend configuration packages

{
  Format = std.contract.from_predicate (fun x =>
    std.is_string x &&
    std.array.elem x ["toml", "json", "yaml", "space_delimited_pairs", "space_delimited_record", "equal_delimited_record", "plaintext"]
  ),

  WhenCondition = {
    os | Array String | optional,
    arch | Array String | optional,
    hostname | Array String | optional,
  },

  # Individual file/folder entry
  FileEntry = {
    name | String | optional,           # Destination filename (defaults to from_file)
    from_file | String | optional,      # External file/folder to copy (plaintext mode)
    from_config | Dyn | optional,       # Inline structured config data
    prefix | Array String | optional,   # Destination prefixes (overrides global)
    format | Format | optional,         # Override format (inferred from name if omitted)
    ignore | Array String | optional,   # Keys to exclude from diff
    when | WhenCondition | optional,    # Per-file condition
    symlink | Bool | optional,          # Create symlink instead of copying (from_file only)
  },

  OrderMeta = {
    files | Array FileEntry,
    prefix | Array String | optional,   # Default prefix for all files
    when | WhenCondition | optional,    # Global condition
    ignore | Array String | optional,   # Global ignore keys
  },

  Order = {
    blend | OrderMeta,
  },
}
