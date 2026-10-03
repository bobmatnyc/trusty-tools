Fixed
- `finish_task` is refused when the test command's captured output states a failure and the model still reports `completed`, and a completion whose command printed no recognisable verdict is marked UNVERIFIED rather than passed. The completion's transcript and event now carry the command's real result lines, capped at 12 lines of 240 characters (#8289).
