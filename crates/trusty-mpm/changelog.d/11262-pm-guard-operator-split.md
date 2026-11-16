Fixed

- pm-guard now ends a word at an unquoted shell redirection or control operator the way bash does before any rule reads the word, so every rule sees the same program, script and file names the shell uses; a segment whose operators it cannot place is refused as unparseable ([#11262](https://github.com/bobmatnyc/trusty-tools/issues/11262))
