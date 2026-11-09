Fixed
- A verification-record or journal append refuses a leaf that was swapped for a symbolic link after the path check, and neither appends to nor changes the mode of the link target (#10746).
- `Bundle::verify` refuses an ID that differs only by case or Unicode normalisation from an existing file with `Conflict { reason: "fold_collision" }`, as `put` does, and writes no second verification record (#10746).
- `move_concept` lists a link in a `.md` file that is not UTF-8 in `MoveReport.unmoved` with reason `unreadable` instead of skipping the file, refuses to move a source that is not UTF-8, and stops on any other read error (#10746).
- `put` and `verify` refuse an actor whose text holds a control character, newline included, with `Conflict { reason: "actor_control_character" }`, so the actor text can no longer forge `log.md` entries or date headings (#10746).
- `move_concept` refuses a source whose frontmatter does not parse with `Parse` before any write, instead of writing an empty concept at the new ID and leaving the bundle in `RecoveryPending` (#10746).
- `deprecate` and `supersede` refuse a `reason` that holds a control character with `Conflict { reason: "reason_control_character" }`, closing the same `log.md` forgery through the reason text (#10746).
