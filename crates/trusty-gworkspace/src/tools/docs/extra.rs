//! Extended Google Docs tool definitions (parity with the Python upstream).
//!
//! Why: The base `docs` group covers create/read/edit/format/table basics; this
//! module adds the tabs, list, image, deep-table, header/footer and template
//! tools so the Rust server reaches feature parity with `gworkspace-mcp`.
//! What: Appends the extended Docs tool group to the shared registry vector.
//! Test: Covered via `tool_list_response()` in `tools::tests`.

use crate::tools::schema::{account_schema, action_enum, tool};
use serde_json::{Value, json};

/// Append the extended Docs tools to the registry.
///
/// Why: Keeps the parity tools grouped and off the base `docs/mod.rs` file to
/// respect the SLOC cap.
/// What: Pushes tabs/paragraph/list/image/deep-table/header-footer/template tools.
/// Test: Covered via `tool_list_response()` in `tools::tests`.
pub(super) fn append(tools: &mut Vec<Value>) {
    // ---- Tabs ----
    tools.push(tool(
        "manage_document_tabs",
        "Manage tabs in a Google Doc: list, get_content, create, update (title/icon), or move (reparent/reorder).",
        json!({
            "account": account_schema(),
            "action": action_enum(&["list", "get_content", "create", "update", "move"]),
            "document_id": { "type": "string" },
            "tab_id": { "type": "string", "description": "Required for get_content, update, move." },
            "title": { "type": "string", "description": "Tab title (create; optional for update)." },
            "icon_emoji": { "type": "string", "description": "Icon emoji (create/update, optional)." },
            "parent_tab_id": { "type": "string", "description": "Parent tab for nesting (create)." },
            "index": { "type": "integer", "description": "Position index (create)." },
            "new_parent_tab_id": { "type": "string", "description": "Move target parent; empty string = root." },
            "new_index": { "type": "integer", "description": "Move target position index." },
        }),
        &["action", "document_id"],
    ));
    tools.push(tool(
        "create_document_tab",
        "Create a new tab in a Google Doc with a title and optional icon/parent/index.",
        json!({
            "account": account_schema(),
            "document_id": { "type": "string" },
            "title": { "type": "string" },
            "icon_emoji": { "type": "string" },
            "parent_tab_id": { "type": "string" },
            "index": { "type": "integer" },
        }),
        &["document_id", "title"],
    ));

    // ---- Paragraph / list ----
    tools.push(tool(
        "move_paragraph_in_document",
        "Move a paragraph (cut-and-paste) from a source index range to a destination index in a Google Doc.",
        json!({
            "account": account_schema(),
            "document_id": { "type": "string" },
            "source_start_index": { "type": "integer" },
            "source_end_index": { "type": "integer" },
            "destination_index": { "type": "integer" },
        }),
        &["document_id", "source_start_index", "source_end_index", "destination_index"],
    ));
    tools.push(tool(
        "format_paragraph_in_document",
        "Apply paragraph-level formatting (named/heading style, alignment, indentation, spacing) to a range.",
        json!({
            "account": account_schema(),
            "document_id": { "type": "string" },
            "start_index": { "type": "integer" },
            "end_index": { "type": "integer" },
            "heading_style": { "type": "string", "description": "NORMAL_TEXT, HEADING_1..6, TITLE, SUBTITLE." },
            "alignment": { "type": "string", "enum": ["START", "CENTER", "END", "JUSTIFIED"] },
            "indent_first_line_pt": { "type": "number" },
            "indent_start_pt": { "type": "number" },
            "space_above_pt": { "type": "number" },
            "space_below_pt": { "type": "number" },
        }),
        &["document_id", "start_index", "end_index"],
    ));
    tools.push(tool(
        "create_list_in_document",
        "Create a bulleted or numbered list at a given index in a Google Doc.",
        json!({
            "account": account_schema(),
            "document_id": { "type": "string" },
            "insert_index": { "type": "integer" },
            "list_type": { "type": "string", "enum": ["BULLETED", "NUMBERED"] },
            "items": { "type": "array", "items": { "type": "string" } },
        }),
        &["document_id", "insert_index", "list_type", "items"],
    ));

    // ---- Images ----
    tools.push(tool(
        "insert_image_in_document",
        "Insert an inline image (from a public https URL) at a given index, with optional width/height in points.",
        json!({
            "account": account_schema(),
            "document_id": { "type": "string" },
            "insert_index": { "type": "integer" },
            "image_uri": { "type": "string", "description": "Publicly accessible image URL." },
            "width_pt": { "type": "number" },
            "height_pt": { "type": "number" },
        }),
        &["document_id", "insert_index", "image_uri"],
    ));

    // ---- Deep table styling ----
    tools.push(tool(
        "format_table_cells",
        "Apply padding/border/background/content-alignment to a cell range. Use row_index=-1 or column_index=-1 to target all rows/columns.",
        json!({
            "account": account_schema(),
            "document_id": { "type": "string" },
            "table_start_index": { "type": "integer" },
            "row_index": { "type": "integer", "description": "0-based; -1 = all rows." },
            "column_index": { "type": "integer", "description": "0-based; -1 = all columns." },
            "num_rows": { "type": "integer", "description": "Required when row_index=-1." },
            "num_columns": { "type": "integer", "description": "Required when column_index=-1." },
            "padding": {
                "type": "object",
                "properties": {
                    "top": { "type": "number" }, "bottom": { "type": "number" },
                    "left": { "type": "number" }, "right": { "type": "number" },
                },
            },
            "border": {
                "type": "object",
                "properties": {
                    "color": { "type": "object", "description": "{red,green,blue} 0..1." },
                    "width": { "type": "number" },
                    "dash_style": { "type": "string", "enum": ["SOLID", "DOT", "DASH"] },
                    "sides": { "type": "array", "items": { "type": "string", "enum": ["top", "bottom", "left", "right"] } },
                },
            },
            "background_color": { "type": "object", "description": "{red,green,blue} 0..1." },
            "content_alignment": { "type": "string", "enum": ["TOP", "MIDDLE", "BOTTOM"] },
        }),
        &["document_id", "table_start_index", "row_index", "column_index"],
    ));
    tools.push(tool(
        "set_table_column_widths",
        "Set explicit column widths (PT; null = evenly distributed) or auto-balance from cell data.",
        json!({
            "account": account_schema(),
            "document_id": { "type": "string" },
            "table_start_index": { "type": "integer" },
            "column_widths": { "type": "array", "items": { "type": ["number", "null"] } },
            "auto_balance": { "type": "boolean" },
            "data": { "type": "array", "items": { "type": "array", "items": { "type": "string" } }, "description": "Required when auto_balance=true." },
            "available_width": { "type": "number", "description": "Usable page width in PT (default 468)." },
            "font_size": { "type": "number", "description": "Font size in PT for the balance algorithm (default 11)." },
            "min_col_width": { "type": "number", "description": "Minimum column width in PT (default 60)." },
            "algorithm": { "type": "string", "enum": ["equalize", "sqrt", "proportional"] },
        }),
        &["document_id", "table_start_index"],
    ));
    tools.push(tool(
        "apply_table_style",
        "Apply a named style preset (minimal, bordered, striped, professional, plain) or custom overrides to an entire table.",
        json!({
            "account": account_schema(),
            "document_id": { "type": "string" },
            "table_start_index": { "type": "integer" },
            "num_rows": { "type": "integer" },
            "num_columns": { "type": "integer" },
            "preset": { "type": "string", "enum": ["minimal", "bordered", "striped", "professional", "plain"] },
            "header_row": { "type": "boolean" },
            "custom": { "type": "object", "description": "Override preset fields (header_background, header_text_bold, odd/even_row_background, border_color, border_width, border_dash_style, cell_padding)." },
        }),
        &["document_id", "table_start_index", "num_rows", "num_columns"],
    ));
    tools.push(tool(
        "format_document_tables",
        "Post-processing pass: apply borders, a styled header row, and content-aware column widths to ALL tables in a Google Doc.",
        json!({ "account": account_schema(), "document_id": { "type": "string" } }),
        &["document_id"],
    ));

    // ---- Header / footer ----
    tools.push(tool(
        "manage_document_header_footer",
        "Manage headers/footers: get, create_header, create_footer, update_header, update_footer, delete_header, delete_footer.",
        json!({
            "account": account_schema(),
            "action": action_enum(&[
                "get", "create_header", "create_footer",
                "update_header", "update_footer", "delete_header", "delete_footer",
            ]),
            "document_id": { "type": "string" },
            "header_id": { "type": "string", "description": "Required for update_header/delete_header." },
            "footer_id": { "type": "string", "description": "Required for update_footer/delete_footer." },
            "text": { "type": "string", "description": "Required for update_header/update_footer." },
        }),
        &["action", "document_id"],
    ));

    // ---- Templates / named styles ----
    tools.push(tool(
        "create_document_from_template",
        "Create a Google Doc by copying a template, replacing {{PLACEHOLDER}} strings from a replacements map.",
        json!({
            "account": account_schema(),
            "template_id": { "type": "string" },
            "title": { "type": "string" },
            "replacements": { "type": "object", "description": "Map of placeholder name (without braces) to value; each KEY is matched as {{KEY}}." },
            "destination_folder_id": { "type": "string" },
        }),
        &["template_id", "title"],
    ));
    tools.push(tool(
        "get_document_named_styles",
        "Get the named style definitions (NORMAL_TEXT, TITLE, SUBTITLE, HEADING_1..6) for a Google Doc.",
        json!({ "account": account_schema(), "document_id": { "type": "string" } }),
        &["document_id"],
    ));
    tools.push(tool(
        "update_document_named_styles",
        "Update one or more named style definitions (text and/or paragraph formatting) in a Google Doc.",
        json!({
            "account": account_schema(),
            "document_id": { "type": "string" },
            "styles": {
                "type": "array",
                "description": "List of {named_style_type, text_style?, paragraph_style?} updates.",
                "items": {
                    "type": "object",
                    "properties": {
                        "named_style_type": { "type": "string", "enum": [
                            "NORMAL_TEXT", "TITLE", "SUBTITLE",
                            "HEADING_1", "HEADING_2", "HEADING_3",
                            "HEADING_4", "HEADING_5", "HEADING_6",
                        ] },
                        "text_style": { "type": "object", "description": "bold, italic, underline, font_size, font_family, text_color{red,green,blue}." },
                        "paragraph_style": { "type": "object", "description": "alignment, line_spacing, space_above, space_below." },
                    },
                    "required": ["named_style_type"],
                },
            },
        }),
        &["document_id", "styles"],
    ));
}
