Fixed

- `catchup::session_finder::parse_filename_timestamp` no longer panics on a
  session filename stem containing a multi-byte UTF-8 character
  ([#5294](https://github.com/bobmatnyc/trusty-tools/issues/5294)). Its
  length guards checked byte length, not char count, so a stem like
  `"123é456-142030"` could pass the `len() == 15` / `len() == 8` checks while
  a fixed-byte-offset slice still landed mid-character and panicked. The
  function now rejects any stem whose date/time parts are not all ASCII
  digits before slicing.
