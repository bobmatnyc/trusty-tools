Changed
- The claude-code runner's spawn and the test-only `test_env::spawn_script` helper now retry `ETXTBSY` through `trusty_common::spawn_retry` instead of carrying two local copies of the same bounded retry. Same budget (3 attempts, 5 ms doubling), same behavior on every other error ([#5446](https://github.com/bobmatnyc/trusty-tools/issues/5446))
