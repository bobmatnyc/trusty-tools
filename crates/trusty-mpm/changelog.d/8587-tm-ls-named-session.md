Added

- `tm ls`: Ctrl-N on a project in the new-session list opens a name step.
  The typed name is slugged the same way as the picker's `n <name>` ("Auth
  Refactor" becomes `tm-auth-refactor-NN`), and the overlay shows that preview
  as you type. Enter creates the session under that name; Esc goes back to the
  list with the filter and selection unchanged. Enter on a project still
  creates a default-named session. Refs #8587.
