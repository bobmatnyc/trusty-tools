Documentation

- One broken intra-doc link on `daemon::doctor::run_doctor_for_manager`, which
  failed the pre-publish rustdoc gate (`scripts/check_rustdoc_links.sh`).
  `SessionManager` is referred to by its full path in the signature and never
  imported, so the doc link now says `crate::session_manager::SessionManager`.
