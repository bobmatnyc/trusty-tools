Fixed
- A verification-record or journal append refuses a leaf that was swapped for a symbolic link after the path check, and neither appends to nor changes the mode of the link target (#10746).
- `Bundle::verify` refuses an ID that differs only by case or Unicode normalisation from an existing file with `Conflict { reason: "fold_collision" }`, as `put` does, and writes no second verification record (#10746).
