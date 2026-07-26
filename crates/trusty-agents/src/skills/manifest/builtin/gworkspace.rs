//! Built-in skills for the Google Workspace (`gworkspace`) tool surface.
//!
//! Why: The owner's naming exemplar — *"gworkspace is a Google Workspace
//! skill"* — read under the 1:1 rule: every gworkspace tool gets its own skill,
//! and every one of those skills is NAMED in Google Workspace terms ("Gmail
//! Search", "Drive File Sharing") rather than echoing `search_gmail_messages`.
//! Every row here carries [`GOOGLE_OAUTH`], so a card can never imply Gmail
//! works on a machine with no authorized account.
//!
//! These tools arrive over MCP from the first-party `trusty-gworkspace` crate,
//! not from this crate's in-process registry, so their coverage is NOT compile-
//! time enforced here. What IS enforced is that every gworkspace tool granted by
//! a checked-in `agent.toml` has a skill — see
//! `agent_roster_grants_only_tools_that_have_a_skill`.
//! What: A `const` table of one-tool [`SkillDef`] rows.
//! Test: `super::super::tests::agent_roster_grants_only_tools_that_have_a_skill`.

use super::super::{SkillDef, SkillKind::Action, tool_skill};
use super::GOOGLE_OAUTH;

/// Shorthand: every row in this file is an `Action` needing Google OAuth.
const fn g(
    id: &'static str,
    name: &'static str,
    description: &'static str,
    tool: &'static str,
) -> SkillDef {
    tool_skill(id, name, description, tool, Action, Some(&GOOGLE_OAUTH))
}

pub(super) static TABLE: &[SkillDef] = &[
    // --- accounts ---------------------------------------------------------
    g(
        "gw-account-list",
        "Google Account List",
        "List the Google Workspace account profiles configured on this machine.",
        "list_accounts",
    ),
    g(
        "gw-account-add",
        "Google Account Authorization",
        "Authorize a new Google Workspace account through OAuth consent.",
        "add_account",
    ),
    g(
        "gw-account-remove",
        "Google Account Removal",
        "Remove a configured Google Workspace account profile locally.",
        "remove_account",
    ),
    g(
        "gw-account-default",
        "Google Default Account",
        "Choose which Google account profile tools use by default.",
        "set_default_account",
    ),
    // --- gmail ------------------------------------------------------------
    g(
        "gmail-search",
        "Gmail Search",
        "Find messages in Gmail using Gmail query syntax.",
        "search_gmail_messages",
    ),
    g(
        "gmail-read",
        "Gmail Message Reader",
        "Read the full content of one Gmail message.",
        "get_gmail_message_content",
    ),
    g(
        "gmail-compose",
        "Gmail Compose and Send",
        "Send, draft or reply to mail from the user's Gmail account.",
        "compose_email",
    ),
    g(
        "gmail-format",
        "Gmail HTML Formatting",
        "Turn markdown-flavoured text into an HTML mail body.",
        "format_email_content",
    ),
    g(
        "gmail-labels-apply",
        "Gmail Labelling",
        "Add or remove labels across a set of Gmail messages.",
        "modify_gmail_messages",
    ),
    g(
        "gmail-labels-manage",
        "Gmail Label Management",
        "Create, rename or delete Gmail labels.",
        "manage_gmail_labels",
    ),
    g(
        "gmail-filters",
        "Gmail Filters",
        "List, create or delete Gmail filters.",
        "manage_gmail_filters",
    ),
    g(
        "gmail-settings",
        "Gmail Settings",
        "Read or change Gmail account settings such as vacation responder and forwarding.",
        "manage_gmail_settings",
    ),
    g(
        "gmail-attachments-list",
        "Gmail Attachment Listing",
        "Enumerate the attachments on a Gmail message.",
        "list_message_attachments",
    ),
    g(
        "gmail-attachment-download",
        "Gmail Attachment Download",
        "Download one Gmail attachment, optionally to disk.",
        "download_gmail_attachment",
    ),
    // `create_draft` predates `compose_email`'s draft action but is still
    // granted by the checked-in roster, so it needs its own skill rather than a
    // gap in that agent's pane.
    g(
        "gmail-draft",
        "Gmail Draft Creation",
        "Save a message as a Gmail draft without sending it.",
        "create_draft",
    ),
    // --- calendar ---------------------------------------------------------
    g(
        "gcal-calendars",
        "Google Calendar Management",
        "Create, read, update or delete the user's calendars.",
        "manage_calendars",
    ),
    g(
        "gcal-events",
        "Google Calendar Events",
        "Create, read, update or delete events on a calendar.",
        "manage_events",
    ),
    g(
        "gcal-free-busy",
        "Google Calendar Availability",
        "Check free/busy availability across calendars for a time range.",
        "query_free_busy",
    ),
    // --- drive ------------------------------------------------------------
    g(
        "gdrive-browse",
        "Google Drive Browser",
        "List the contents of a Drive folder.",
        "list_drive_contents",
    ),
    g(
        "gdrive-search",
        "Google Drive Search",
        "Search Drive for files matching a query.",
        "search_drive_files",
    ),
    g(
        "gdrive-read",
        "Google Drive File Reader",
        "Fetch a Drive file's content, exporting Google-native docs as text.",
        "get_drive_file_content",
    ),
    g(
        "gdrive-shared-drives",
        "Google Shared Drives",
        "List the shared drives the account can reach.",
        "list_shared_drives",
    ),
    g(
        "gdrive-manage",
        "Google Drive File Management",
        "Create, rename, move, copy, trash or upload files in Drive.",
        "manage_drive_file",
    ),
    g(
        "gdrive-permissions",
        "Google Drive Sharing",
        "List or change the sharing permissions on a Drive file.",
        "manage_file_permissions",
    ),
    g(
        "gdrive-sync",
        "Google Drive Sync",
        "Synchronise a Drive folder with local content.",
        "sync_drive",
    ),
    // --- docs: content ----------------------------------------------------
    g(
        "gdocs-create",
        "Google Docs Creation",
        "Create a new empty Google Doc.",
        "create_document",
    ),
    g(
        "gdocs-read",
        "Google Docs Reader",
        "Fetch a Google Doc's full content.",
        "get_document",
    ),
    g(
        "gdocs-outline",
        "Google Docs Outline",
        "Read a Google Doc's structural outline without inline formatting.",
        "get_document_structure",
    ),
    g(
        "gdocs-append",
        "Google Docs Append",
        "Append text to the end of a Google Doc.",
        "append_to_document",
    ),
    g(
        "gdocs-insert-text",
        "Google Docs Text Insertion",
        "Insert text at a specific position in a Google Doc.",
        "insert_text_in_document",
    ),
    g(
        "gdocs-replace-text",
        "Google Docs Find and Replace",
        "Replace every occurrence of a string in a Google Doc.",
        "replace_text_in_document",
    ),
    g(
        "gdocs-delete-range",
        "Google Docs Range Deletion",
        "Delete a content range from a Google Doc.",
        "delete_range_in_document",
    ),
    g(
        "gdocs-move-paragraph",
        "Google Docs Paragraph Move",
        "Move a paragraph to another position in a Google Doc.",
        "move_paragraph_in_document",
    ),
    g(
        "gdocs-comments",
        "Google Docs Comments",
        "List, create, reply to, resolve or delete comments on a Google Doc.",
        "manage_document_comments",
    ),
    g(
        "gdocs-convert",
        "Google Docs Conversion",
        "Convert a document between Google Docs and another format.",
        "convert_document",
    ),
    g(
        "gdocs-publish-markdown",
        "Google Docs Markdown Publishing",
        "Publish a Markdown document into a formatted Google Doc.",
        "publish_markdown_to_doc",
    ),
    g(
        "gdocs-render-mermaid",
        "Google Docs Diagram Rendering",
        "Render a Mermaid diagram into a Google Doc as an image.",
        "render_mermaid_to_doc",
    ),
    // --- docs: formatting -------------------------------------------------
    g(
        "gdocs-format-range",
        "Google Docs Text Formatting",
        "Apply bold, italic, size or a named style to a range in a Google Doc.",
        "format_document_range",
    ),
    g(
        "gdocs-format-paragraph",
        "Google Docs Paragraph Formatting",
        "Apply heading style, alignment, indentation or spacing to a paragraph range.",
        "format_paragraph_in_document",
    ),
    g(
        "gdocs-doc-style",
        "Google Docs Page Setup",
        "Change document-level style such as page size and margins.",
        "set_document_style",
    ),
    g(
        "gdocs-named-styles-read",
        "Google Docs Named Styles",
        "Read a Google Doc's named style definitions.",
        "get_document_named_styles",
    ),
    g(
        "gdocs-named-styles-write",
        "Google Docs Named Style Editing",
        "Update a Google Doc's named style definitions.",
        "update_document_named_styles",
    ),
    g(
        "gdocs-lists",
        "Google Docs Lists",
        "Create a bulleted or numbered list in a Google Doc.",
        "create_list_in_document",
    ),
    g(
        "gdocs-images",
        "Google Docs Images",
        "Insert an image into a Google Doc from a public URL.",
        "insert_image_in_document",
    ),
    g(
        "gdocs-header-footer",
        "Google Docs Headers and Footers",
        "Create, update or delete a Google Doc's headers and footers.",
        "manage_document_header_footer",
    ),
    g(
        "gdocs-template",
        "Google Docs Templating",
        "Create a Google Doc from a template, substituting placeholders.",
        "create_document_from_template",
    ),
    // --- docs: tabs + tables ----------------------------------------------
    g(
        "gdocs-tabs",
        "Google Docs Tabs",
        "List, read, update or move the tabs of a Google Doc.",
        "manage_document_tabs",
    ),
    g(
        "gdocs-tab-create",
        "Google Docs Tab Creation",
        "Create a new tab in a Google Doc.",
        "create_document_tab",
    ),
    g(
        "gdocs-table-insert",
        "Google Docs Table Insertion",
        "Insert a table into a Google Doc.",
        "insert_table_in_document",
    ),
    g(
        "gdocs-table-find",
        "Google Docs Table Listing",
        "Enumerate the tables in a Google Doc.",
        "find_tables_in_document",
    ),
    g(
        "gdocs-table-structure",
        "Google Docs Table Structure",
        "Insert or delete a row or column in a Google Doc table.",
        "manage_table_structure",
    ),
    g(
        "gdocs-table-cells",
        "Google Docs Table Cell Formatting",
        "Apply padding, borders, background or alignment to table cells.",
        "format_table_cells",
    ),
    g(
        "gdocs-table-widths",
        "Google Docs Table Column Widths",
        "Set or auto-balance a Google Doc table's column widths.",
        "set_table_column_widths",
    ),
    g(
        "gdocs-table-style",
        "Google Docs Table Styling",
        "Apply a named table style preset to a Google Doc table.",
        "apply_table_style",
    ),
    g(
        "gdocs-tables-format-all",
        "Google Docs Table Cleanup",
        "Restyle every table in a Google Doc in one pass.",
        "format_document_tables",
    ),
    // --- sheets -----------------------------------------------------------
    g(
        "gsheets-read",
        "Google Sheets Reader",
        "Fetch a spreadsheet's metadata and optionally its grid data.",
        "get_spreadsheet",
    ),
    g(
        "gsheets-manage",
        "Google Sheets Management",
        "Create a spreadsheet or add and delete sheets within one.",
        "manage_spreadsheet",
    ),
    g(
        "gsheets-values",
        "Google Sheets Values",
        "Read, write, append or clear cell values in a sheet range.",
        "modify_sheet_values",
    ),
    g(
        "gsheets-format",
        "Google Sheets Formatting",
        "Format cells, number formats, merges and column widths in a sheet.",
        "format_sheet",
    ),
    g(
        "gsheets-chart",
        "Google Sheets Charts",
        "Add a bar, line, area or pie chart over a data range.",
        "create_chart",
    ),
    // --- slides -----------------------------------------------------------
    g(
        "gslides-read",
        "Google Slides Reader",
        "Read presentations, decks, individual slides or all slide text.",
        "get_slides",
    ),
    g(
        "gslides-manage",
        "Google Slides Management",
        "Create a presentation, add or delete slides, or replace element text.",
        "manage_slides",
    ),
    g(
        "gslides-content",
        "Google Slides Content",
        "Add a text box, image or bulleted-list slide to a presentation.",
        "add_slide_content",
    ),
    g(
        "gslides-format",
        "Google Slides Formatting",
        "Apply formatting to a slide's elements.",
        "format_slide",
    ),
    // --- tasks ------------------------------------------------------------
    g(
        "gtasks-lists",
        "Google Tasks Lists",
        "Create, read, update or delete Google Tasks lists.",
        "manage_task_lists",
    ),
    g(
        "gtasks-manage",
        "Google Tasks Management",
        "Create, update, move, search or complete tasks within a list.",
        "manage_tasks",
    ),
    g(
        "gtasks-list",
        "Google Tasks Listing",
        "List the tasks on the default Google Tasks list.",
        "list_tasks",
    ),
    g(
        "gtasks-complete",
        "Google Tasks Completion",
        "Mark one Google Task as completed.",
        "complete_task",
    ),
    // As with `create_draft`: superseded by `manage_tasks`, still granted.
    g(
        "gtasks-create",
        "Google Tasks Creation",
        "Add a new task to a Google Tasks list.",
        "create_task",
    ),
];
