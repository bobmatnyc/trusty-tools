Changed

- The signed-install script prints `trusty-search service install` as the
  restart step instead of a hand-run `launchctl bootout`/`bootstrap` pair
  against a plist path it guessed. Its path resolver had picked
  `com.trusty.trusty-search.plist` as canonical and labelled the live
  `com.trusty.search` a drifted alias — the reverse of the truth (#4868)
