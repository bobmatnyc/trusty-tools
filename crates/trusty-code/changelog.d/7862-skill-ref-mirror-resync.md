Fixed

- The embedded `condition-based-waiting` and `verification-before-completion` skill references are byte copies of trusty-mpm's bundled skills again, so a composed agent reads the current text instead of the pre-#8323 one (#7862).
