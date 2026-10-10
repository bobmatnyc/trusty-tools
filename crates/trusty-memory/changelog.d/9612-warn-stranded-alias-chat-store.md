Added

- The daemon now warns once per alias when a chat store written before the alias redirect (#9544) still sits in the alias's own directory (`<root>/<alias>/chat_sessions.redb`). The warning names the file, its size and the target palace, and tells the operator to back the file up, then merge it by hand or delete it. The file is never opened or changed. Refs [#9612](https://github.com/bobmatnyc/trusty-tools/issues/9612)
