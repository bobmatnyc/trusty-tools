Changed

- `tm fleet init` still keeps an Architect file whose bytes match no shipped version (an owner or Architect edit), and now also writes the new shipped copy beside it as `<file>.shipped`. The init report names the sibling path, so a release change to an edited file is visible and can be merged by hand. An unedited file still updates in place.
