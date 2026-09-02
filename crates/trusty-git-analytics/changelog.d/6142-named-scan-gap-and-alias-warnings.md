Fixed

- `tga audit` names a failed unmerged-identity scan as a gap on the DD manifest instead of dropping the identity-merge risk flag with only a log line, so bus factor and top-author share never print with an unstated check beside them. An `authors.aliases` value that will not parse as JSON is likewise named on stderr — with the author it belongs to — at both the collect and report sites, rather than silently reading as "this author has no confirmed merges". (#6142)
