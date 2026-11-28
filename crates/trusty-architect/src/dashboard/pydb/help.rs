//! `dashboard-db.py`'s argparse usage and help texts, as Python 3.14 prints them (#12029 P2g).
//!
//! Why: a bad command line must print what the frozen script prints.
//! What: generated from the frozen script's `-h` output; [`TOP_HELP`] and,
//! per sub-command in `add_parser` order, its usage block and help.
//! The `status-brief` example path in [`TOP_HELP`] is neutral by design
//! (#12029 P2g): the frozen text names a private brief directory.
//! Test: the `usage-*` and `help-*` recordings in `tests/replay_dashboard_db.rs`,
//! `help_names_a_neutral_status_brief_example`.

pub(super) const TOP_USAGE: &str = r#"usage: dashboard-db.py [-h] [--root ROOT] [--actor ACTOR]
                       {get,list,set,update,delete,pruned,next,create,log,status-brief,summary,requests,validate,fsck,tally,footer,batch} ...
"#;
pub(super) const TOP_HELP: &str = r#"usage: dashboard-db.py [-h] [--root ROOT] [--actor ACTOR]
                       {get,list,set,update,delete,pruned,next,create,log,status-brief,summary,requests,validate,fsck,tally,footer,batch} ...

Read and write Architect dashboard documents.

positional arguments:
  {get,list,set,update,delete,pruned,next,create,log,status-brief,summary,requests,validate,fsck,tally,footer,batch}
    status-brief        prepend a status brief to a task: status-brief T1c
                        MB0be9 sample/notes/x.md
    requests            read-only: owner requests waiting for the Architect
                        (status update, briefing, steer, feedback...), oldest
                        first

options:
  -h, --help            show this help message and exit
  --root ROOT
  --actor ACTOR         change-log actor (jsonl store)
"#;
/// Per sub-command: `(usage, help)`.
pub(super) const SUB_TEXT: [(&str, &str); 17] = [
    (
        r#"usage: dashboard-db.py get [-h] collection id
"#,
        r#"usage: dashboard-db.py get [-h] collection id

positional arguments:
  collection
  id

options:
  -h, --help  show this help message and exit
"#,
    ),
    (
        r#"usage: dashboard-db.py list [-h] [--status STATUS] collection
"#,
        r#"usage: dashboard-db.py list [-h] [--status STATUS] collection

positional arguments:
  collection

options:
  -h, --help       show this help message and exit
  --status STATUS
"#,
    ),
    (
        r#"usage: dashboard-db.py set [-h] --json FILE_OR_STRING
                           [--if-version IF_VERSION] [--allow-id]
                           collection id
"#,
        r#"usage: dashboard-db.py set [-h] --json FILE_OR_STRING
                           [--if-version IF_VERSION] [--allow-id]
                           collection id

positional arguments:
  collection
  id

options:
  -h, --help            show this help message and exit
  --json FILE_OR_STRING
  --if-version IF_VERSION
  --allow-id            silence the new-id warning when restoring a known id
"#,
    ),
    (
        r#"usage: dashboard-db.py update [-h] --json FILE_OR_STRING
                              [--if-version IF_VERSION]
                              collection id
"#,
        r#"usage: dashboard-db.py update [-h] --json FILE_OR_STRING
                              [--if-version IF_VERSION]
                              collection id

positional arguments:
  collection
  id

options:
  -h, --help            show this help message and exit
  --json FILE_OR_STRING
  --if-version IF_VERSION
"#,
    ),
    (
        r#"usage: dashboard-db.py delete [-h] [--if-version IF_VERSION] collection id
"#,
        r#"usage: dashboard-db.py delete [-h] [--if-version IF_VERSION] collection id

positional arguments:
  collection
  id

options:
  -h, --help            show this help message and exit
  --if-version IF_VERSION
"#,
    ),
    (
        r#"usage: dashboard-db.py pruned [-h] [--id ID] [--collection COLLECTION]
"#,
        r#"usage: dashboard-db.py pruned [-h] [--id ID] [--collection COLLECTION]

options:
  -h, --help            show this help message and exit
  --id ID
  --collection COLLECTION
"#,
    ),
    (
        r#"usage: dashboard-db.py next [-h] prefix
"#,
        r#"usage: dashboard-db.py next [-h] prefix

positional arguments:
  prefix      id prefix letter, e.g. D

options:
  -h, --help  show this help message and exit
"#,
    ),
    (
        r#"usage: dashboard-db.py create [-h] --json FILE_OR_STRING collection prefix
"#,
        r#"usage: dashboard-db.py create [-h] --json FILE_OR_STRING collection prefix

positional arguments:
  collection
  prefix

options:
  -h, --help            show this help message and exit
  --json FILE_OR_STRING
"#,
    ),
    (
        r#"usage: dashboard-db.py log [-h] collection id text
"#,
        r#"usage: dashboard-db.py log [-h] collection id text

positional arguments:
  collection
  id
  text

options:
  -h, --help  show this help message and exit
"#,
    ),
    (
        r#"usage: dashboard-db.py status-brief [-h] id brief_id brief
"#,
        r#"usage: dashboard-db.py status-brief [-h] id brief_id brief

positional arguments:
  id
  brief_id
  brief

options:
  -h, --help  show this help message and exit
"#,
    ),
    (
        r#"usage: dashboard-db.py summary [-h]
"#,
        r#"usage: dashboard-db.py summary [-h]

options:
  -h, --help  show this help message and exit
"#,
    ),
    (
        r#"usage: dashboard-db.py requests [-h] [--json]
"#,
        r#"usage: dashboard-db.py requests [-h] [--json]

options:
  -h, --help  show this help message and exit
  --json
"#,
    ),
    (
        r#"usage: dashboard-db.py validate [-h]
"#,
        r#"usage: dashboard-db.py validate [-h]

options:
  -h, --help  show this help message and exit
"#,
    ),
    (
        r#"usage: dashboard-db.py fsck [-h]
"#,
        r#"usage: dashboard-db.py fsck [-h]

options:
  -h, --help  show this help message and exit
"#,
    ),
    (
        r#"usage: dashboard-db.py tally [-h] [--json]
"#,
        r#"usage: dashboard-db.py tally [-h] [--json]

options:
  -h, --help  show this help message and exit
  --json
"#,
    ),
    (
        r#"usage: dashboard-db.py footer [-h] [--json]
"#,
        r#"usage: dashboard-db.py footer [-h] [--json]

options:
  -h, --help  show this help message and exit
  --json
"#,
    ),
    (
        r#"usage: dashboard-db.py batch [-h] file
"#,
        r#"usage: dashboard-db.py batch [-h] file

positional arguments:
  file        JSON file (or - for stdin): list of ops or {ops: [...]}

options:
  -h, --help  show this help message and exit
"#,
    ),
];
