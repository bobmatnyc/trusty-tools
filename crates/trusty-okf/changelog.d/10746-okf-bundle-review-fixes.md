Fixed
- A verification-record or journal append refuses a leaf that was swapped for a symbolic link after the path check, and neither appends to nor changes the mode of the link target (#10746).
- `Bundle::verify` refuses an ID that differs only by case or Unicode normalisation from an existing file with `Conflict { reason: "fold_collision" }`, as `put` does, and writes no second verification record (#10746).
- `move_concept` lists a link in a `.md` file that is not UTF-8 in `MoveReport.unmoved` with reason `unreadable` instead of skipping the file, refuses to move a source that is not UTF-8, and stops on any other read error (#10746).
