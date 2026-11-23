//! Comment-preserving `toml_edit` edits of `config.toml`'s channel tables
//! (#7609 slice 7).
//!
//! Why: `config.toml` is an operator-edited file. Slice 5's global `PUT`
//! removed the deprecated `[[listeners]]` table with a plain
//! `DocumentMut::remove`, and live verification found that it also deleted a
//! seven-line comment block about `tickets-mcp`/ADR-0014 that had nothing to do
//! with listeners. The cause is where `toml_edit` files such a block: the
//! comment lines above a table header are that table's LEADING DECOR, so
//! removing the table removes them too. The same write dropped the heading
//! above `[[channels]]` and moved the array to the top of the file, because a
//! freshly rendered array carries no `Table::position` and `toml_edit` emits a
//! position-less table before every positioned one.
//!
//! What: two operations on a parsed [`toml_edit::DocumentMut`].
//! [`remove_preserving_comments`] hands a removed table's leading decor to the
//! next table in the file — or to the document trailer when it was last — and
//! [`replace_array_of_tables`] re-applies the previous array's leading decor
//! and pins every table of the replacement, nested ones included, to the
//! position the array already had. Both then renumber the whole document so
//! the pinned block lands where it belongs. A comment written INSIDE a removed
//! table is still lost: there is nothing left to attach it to.
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

/// Every table under `table`, depth first in declaration order, as the sort key
/// each one is to be ordered by.
///
/// Why the carry: a table with no position of its own belongs immediately after
/// the last positioned table before it — that is what makes a pinned
/// replacement block stay together rather than being emitted ahead of the whole
/// file.
fn sort_keys(table: &Table, carry: &mut usize, out: &mut Vec<usize>) {
    for (_, item) in table.iter() {
        match item {
            Item::Table(child) => {
                if let Some(position) = child.position() {
                    *carry = position;
                }
                out.push(*carry);
                sort_keys(child, carry, out);
            }
            Item::ArrayOfTables(array) => {
                for child in array.iter() {
                    if let Some(position) = child.position() {
                        *carry = position;
                    }
                    out.push(*carry);
                    sort_keys(child, carry, out);
                }
            }
            _ => {}
        }
    }
}

/// Write `ranks` back onto the same depth-first traversal [`sort_keys`] read.
fn apply_ranks(table: &mut Table, ranks: &[usize], next: &mut usize) {
    for (_, item) in table.iter_mut() {
        match item {
            Item::Table(child) => {
                child.set_position(ranks[*next]);
                *next += 1;
                apply_ranks(child, ranks, next);
            }
            Item::ArrayOfTables(array) => {
                for child in array.iter_mut() {
                    child.set_position(ranks[*next]);
                    *next += 1;
                    apply_ranks(child, ranks, next);
                }
            }
            _ => {}
        }
    }
}

/// Renumber every table in `document` densely, keeping the order it renders in.
///
/// Why: `toml_edit` orders the whole document by `Table::position`, so an
/// inserted table with no position of its own, or a gap left by a removal, has
/// to be resolved into the sequence before rendering. Sorting by (position,
/// traversal index) is STABLE, so a document whose tables all carry positions
/// renumbers to exactly the order it already had — the operator's file is
/// rewritten byte for byte, verified in
/// `document_tests::a_document_that_needs_no_repositioning_is_rewritten_byte_for_byte`.
fn normalize_positions(document: &mut DocumentMut) {
    let mut keys = Vec::new();
    let mut carry = 0;
    sort_keys(document.as_table(), &mut carry, &mut keys);
    let mut order: Vec<usize> = (0..keys.len()).collect();
    order.sort_by_key(|&index| (keys[index], index));
    let mut ranks = vec![0; keys.len()];
    for (rank, &index) in order.iter().enumerate() {
        ranks[index] = rank;
    }
    let mut next = 0;
    apply_ranks(document.as_table_mut(), &ranks, &mut next);
}

/// Give `table` and every table beneath it the same position.
///
/// Why: a shared position plus the stable sort in [`normalize_positions`] is
/// how a replacement block is placed as ONE contiguous run at the anchor,
/// without needing free integers between the anchor and the table after it.
fn pin(table: &mut Table, at: usize) {
    for (_, item) in table.iter_mut() {
        match item {
            Item::Table(child) => {
                child.set_position(at);
                pin(child, at);
            }
            Item::ArrayOfTables(array) => {
                for child in array.iter_mut() {
                    child.set_position(at);
                    pin(child, at);
                }
            }
            _ => {}
        }
    }
}

/// The lowest position above `after`, over every table in the subtree.
///
/// Why nested tables count: the table rendered next after a removed one is
/// whichever has the next position, and that is frequently a sub-table of an
/// EARLIER root key — `[[mcp.services]]` sits between `[[listeners]]` and
/// `[[channels]]` in the operator's own file. A root-only search hands the
/// salvaged comment to `[[channels]]` and the file then reads as if the note
/// were about channels.
fn next_position_after(table: &Table, after: usize) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut consider = |child: &Table| {
        if let Some(position) = child.position()
            && position > after
            && best.is_none_or(|current| position < current)
        {
            best = Some(position);
        }
        if let Some(found) = next_position_after(child, after) {
            best = Some(best.map_or(found, |current| current.min(found)));
        }
    };
    for (_, item) in table.iter() {
        match item {
            Item::Table(child) => consider(child),
            Item::ArrayOfTables(array) => array.iter().for_each(&mut consider),
            _ => {}
        }
    }
    best
}

/// The table rendered at `position`, anywhere in the subtree.
fn table_at_mut(table: &mut Table, position: usize) -> Option<&mut Table> {
    for (_, item) in table.iter_mut() {
        let children: Vec<&mut Table> = match item {
            Item::Table(child) => vec![child],
            Item::ArrayOfTables(array) => array.iter_mut().collect(),
            _ => continue,
        };
        for child in children {
            if child.position() == Some(position) {
                return Some(child);
            }
            if let Some(found) = table_at_mut(child, position) {
                return Some(found);
            }
        }
    }
    None
}

/// Hand `salvage` to the table rendered immediately after `after`.
///
/// What: no table after it means the comment belonged at end of file, so it
/// becomes the document's trailer instead of being dropped. Whitespace-only
/// salvage is discarded — there is no comment in it to save.
fn reattach(document: &mut DocumentMut, after: Option<usize>, salvage: &str) {
    if salvage.trim().is_empty() {
        return;
    }
    let Some(after) = after else {
        return;
    };
    match next_position_after(document.as_table(), after) {
        Some(position) => {
            if let Some(table) = table_at_mut(document.as_table_mut(), position) {
                let existing = table
                    .decor()
                    .prefix()
                    .and_then(|raw| raw.as_str())
                    .unwrap_or_default()
                    .to_string();
                table
                    .decor_mut()
                    .set_prefix(format!("{}\n{existing}", salvage.trim_end_matches('\n')));
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
    normalize_positions(document);
    true
}

/// Replace `key`'s array of tables, keeping its header comment and its place.
///
/// Why: assigning a freshly rendered array drops the comment above the old
/// header and leaves every new table position-less, which `toml_edit` renders
/// BEFORE every positioned table — so a save silently moved `[[channels]]` to
/// the top of the operator's file and dropped its heading. The nested tables
/// `toml` renders for a channel's two filters are the same hazard one level
/// down, which is why [`pin`] descends.
/// What: the previous array's leading decor is re-applied and the whole
/// replacement is pinned to the position the array already had, so an entry
/// added or removed by this write never splits the block or moves it. An array
/// the document did not declare before is pinned past every existing table and
/// therefore lands at end of file.
/// Test: `document_tests::a_replaced_array_keeps_its_comment_and_place`,
/// `document_tests::a_grown_array_stays_one_block_where_it_was`.
pub(crate) fn replace_array_of_tables(document: &mut DocumentMut, key: &str, mut rendered: Item) {
    let existing = document.get(key);
    let anchor = existing.and_then(first_position);
    let salvage = existing.and_then(leading);
    // A key the document never declared has no anchor to inherit; pinning it
    // past every real position puts it at end of file, which is where an
    // appended table belongs.
    let at = anchor.unwrap_or(usize::MAX);
    if let Item::ArrayOfTables(array) = &mut rendered {
        for table in array.iter_mut() {
            table.set_position(at);
            pin(table, at);
        }
    }
    // A key the document declared keeps its own heading; a new one gets a blank
    // line, so an appended array does not abut the table above it.
    set_leading(&mut rendered, salvage.unwrap_or_else(|| "\n".to_string()));
    document[key] = rendered;
    normalize_positions(document);
}

#[cfg(test)]
#[path = "document_tests.rs"]
mod document_tests;
