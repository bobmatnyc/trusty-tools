Fixed
- pm-guard's destructive-delete rule no longer refuses a lone `python3`/`python`/`node`/`ruby` or `cat` fed one quoted here-document whose body mentions `find` or `rm` in text that does not lex. A body a shell runs, a capture re-runs, an expansion reaches or a later segment can use is still judged (#7190).
