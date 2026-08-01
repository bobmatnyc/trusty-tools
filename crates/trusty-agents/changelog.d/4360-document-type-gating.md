Added

- **Document-type gating in the trusty-agents Projects surface (issue [#4360](https://github.com/bobmatnyc/trusty-tools/issues/4360)).**
  `lib/projectFiles.ts` now carries `DOCUMENT_TYPES`, an explicit table of every
  document type the surface handles and what each may do: `.md`/`.markdown` are
  **editable**, while `.pdf`, `.docx`, `.xlsx`, `.xls`, `.txt` and `.csv` are
  **view-only** — listed and named, never writable. Anything the table does not
  claim is **unsupported** and refused outright. Only markdown is editable, per
  the parent requirement, which is why `.txt` and `.csv` are view-only despite
  being trivially writable text.
  The table is the single source of truth for both consumers:
  `writeProjectFile` refuses a non-editable path through the same
  `documentKindFor` lookup that `ProjectFilesPanel` uses to decide whether a row
  opens, so what the UI offers and what the client will send cannot drift.
  Refused rows now say *why* — a view-only row is badged with its type name, an
  unsupported one is marked as such — instead of sharing one "only markdown"
  tooltip. The editor's `readonly` prop ([#4359](https://github.com/bobmatnyc/trusty-tools/issues/4359))
  is derived from the table rather than hardcoded, so mounting a view-only
  document in that pane flips the editor to read-only and drops the save
  affordance with no further gating logic.
  No preview rendering ships here: displaying a view-only document is the canvas
  viewer's job ([#4401](https://github.com/bobmatnyc/trusty-tools/issues/4401)),
  which still has open owner questions. `.pptx`, images and `.rst`/`.org` are
  deliberately absent from the table pending the owner call the issue asks for —
  absent means refused, and adding a row later is additive.
