Fixed

- The daemon now flushes its snapshot before exiting on SIGTERM/SIGINT.
  Previously it removed the socket and returned with the batch worker still
  holding up to one write window of applied-but-unwritten documents, which were
  lost: the worker's final flush is gated on the op channel closing, and the
  accept loop holds a sender for the process lifetime. Latent while a daemon
  only ever died at shutdown; routine now that trusty-memory reaps daemons to
  hold its process cap.
