Fixed
- The OKG feed lock is released as soon as its guard drops. Before, a child process forked during a feed pass kept the lock until it ran its new program, so the next pass reported "feed already running" and fed nothing (#11987).
