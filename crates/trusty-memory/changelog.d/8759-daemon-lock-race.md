Fixed

- Two `trusty-memory serve --foreground` daemons started at the same moment on one data root no longer both serve. Previously both could prove the socket free, bind it and run, with one stranded on a socket nothing could reach; now exactly one binds and the other exits with an error naming the socket (refs [#8759](https://github.com/bobmatnyc/trusty-tools/issues/8759))
