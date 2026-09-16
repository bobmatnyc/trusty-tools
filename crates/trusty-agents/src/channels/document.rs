//! Comment-preserving `toml_edit` edits of `config.toml`'s channel tables
//! (#7609 slice 7).
//!
//! Why: `config.toml` is an operator-edited file. Slice 5's global `PUT`
//! removed the deprecated `[[listeners]]` table with a plain
//! `DocumentMut::remove`, and live verification found that it also deleted a
//! seven-line comment block about `tickets-mcp`/ADR-0014 that had nothing to do
//! with listeners. The cause is where `toml_edit` stores such a block: the
//! comment lines above a table header are that table's LEADING DECOR, so
//! removing the table removes them too. The same applies to the header comment
//! above `[[channels]]`, which a wholesale replacement of the array dropped,
//! and to the array's place in the file, which the replacement moved.
//! What: two operations, both on a parsed [`toml_edit::DocumentMut`].
//! [`remove_preserving_comments`] salvages a removed table's leading decor onto
//! the next table in the file (or the document trailer when it was last), and
//! [`replace_array_of_tables`] re-applies the previous array's leading decor
//! and rendered positions to its replacement. A comment written INSIDE a
//! removed table is still lost — there is nothing left to attach it to.
//! Test: `document_tests` — the whole module.

use toml_edit::{DocumentMut, Item, Table};

/// The rendered position of a root item — the first one, for an array of
/// tables.
fn first_position(item: &Item) -> Option<usize> {
    match item {
        Item::Table(table) => table.position(),
        Item::ArrayOfTables(array) => array.iter().filter_map(Table::position).min(),
        _ => None,
    }
}

/// The leading decor (blank lines and comment lines) above an item's header.
fn leading(item: &Item) -> Option<String> {
    let decor = match item {
        Item::Table(table) => table.decor(),
        Item::ArrayOfTables(array) => array.iter().next()?.decor(),
        _ => return None,
    };
    decor
        .prefix()
        .and_then(|raw| raw.as_str())
        .map(str::to_owned)
}

fn set_leading(item: &mut Item, text: String) {
    let decor = match item {
        Item::Table(table) => table.decor_mut(),
        Item::ArrayOfTables(array) => match array.iter_mut().next() {
            Some(table) => table.decor_mut(),
            None => return,
        },
        _ => return,
    };
    decor.set_prefix(text);
}

/// Put `salvage` above whatever decor `item` already carries, keeping one blank
/// line between the two blocks.
fn prepend_leading(item: &mut Item, salvage: &str) {
    match leading(item) {
        Some(existing) => {
            let joined = format!("{}\n{existing}", salvage.trim_end_matches('\n'));
            set_leading(item, joined);
        }
        None => set_leading(item, salvage.to_string()),
    }
}

/// Every table in `table`'s subtree, nested ones included.
///
/// Why: `toml_edit` numbers positions across the WHOLE document, nested tables
/// included, so a position past every existing one has to be derived from that
/// total rather than from the root item count.
fn table_count(table: &Table) -> usize {
    table
        .iter()
        .map(|(_, item)| match item {
            Item::Table(child) => 1 + table_count(child),
            Item::ArrayOfTables(array) => array
                .iter()
                .map(|child| 1 + table_count(child))
                .sum::<usize>(),
            _ => 0,
        })
        .sum()
}

/// Hand `salvage` to the first root item rendered after `after`.
///
/// What: no item after it means the comment belonged at end of file, so it
/// becomes the document's trailer instead of being dropped. Whitespace-only
/// salvage is discarded — there is no comment in it to save.
fn reattach(document: &mut DocumentMut, after: Option<usize>, salvage: &str) {
    if salvage.trim().is_empty() {
        return;
    }
    let Some(after) = after else {
        return;
    };
    let mut next: Option<(usize, String)> = None;
    for (key, item) in document.iter() {
        let Some(position) = first_position(item) else {
            continue;
        };
        if position > after && next.as_ref().is_none_or(|(best, _)| position < *best) {
            next = Some((position, key.to_string()));
        }
    }
    match next {
        Some((_, key)) => {
            if let Some(item) = document.get_mut(&key) {
                prepend_leading(item, salvage);
            }
        }
        None => {
            let trailing = document.trailing().as_str().unwrap_or_default().to_string();
            document.set_trailing(format!("{salvage}{trailing}"));
        }
    }
}

/// Remove `key` from `document` without taking the comment block above it.
///
/// Why: see the module doc — this is the slice 5 defect, whose live symptom was
/// a `PUT /api/channels` that deleted an operator's `tickets-mcp` note along
/// with the `[[listeners]]` table it sat above.
/// What: the removed item's leading decor moves to the next table in the file,
/// or to the document trailer when nothing follows it. Returns whether `key`
/// was there at all.
/// Test: `document_tests::removing_a_table_keeps_the_comment_above_it`,
/// `document_tests::a_trailing_table_hands_its_comment_to_the_trailer`.
pub(crate) fn remove_preserving_comments(document: &mut DocumentMut, key: &str) -> bool {
    let Some(item) = document.get(key) else {
        return false;
    };
    let salvage = leading(item).unwrap_or_default();
    let at = first_position(item);
    document.remove(key);
    reattach(document, at, &salvage);
    true
}

/// Replace `key`'s array of tables, keeping its header comment and its place.
///
/// Why: assigning a freshly rendered array drops the comment above the old
/// header and leaves every new table position-less, which `toml_edit` renders
/// BEFORE every positioned table — so a save silently moved `[[channels]]` to
/// the top of the operator's file and dropped its heading.
/// What: the previous array's leading decor is re-applied, and each replacement
/// table takes the position of the entry it replaces. A list that GREW has no
/// position to inherit for its new entries, so the whole array is renumbered
/// past every existing table and renders at end of file as one contiguous
/// block — never split across the tables it used to sit among.
/// Test: `document_tests::a_replaced_array_keeps_its_comment_and_place`,
/// `document_tests::a_grown_array_is_rendered_as_one_block_at_the_end`.
pub(crate) fn replace_array_of_tables(document: &mut DocumentMut, key: &str, mut rendered: Item) {
    let base = table_count(document.as_table()) + 1;
    let (previous, salvage) = match document.get(key) {
        Some(item) => (
            match item {
                Item::ArrayOfTables(array) => array.iter().map(Table::position).collect(),
                _ => Vec::new(),
            },
            leading(item),
        ),
        None => (Vec::new(), None),
    };
    if let Item::ArrayOfTables(array) = &mut rendered {
        let inherit = array.len() <= previous.len();
        for (index, table) in array.iter_mut().enumerate() {
            let position = if inherit {
                previous.get(index).copied().flatten()
            } else {
                None
            };
            table.set_position(position.unwrap_or(base + index));
        }
    }
    if let Some(salvage) = salvage {
        set_leading(&mut rendered, salvage);
    }
    document[key] = rendered;
}

#[cfg(test)]
#[path = "document_tests.rs"]
mod document_tests;
