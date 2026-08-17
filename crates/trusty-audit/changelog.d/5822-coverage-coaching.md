Added

- The guided flow and the `add` / `targets` help now coach registration breadth
  rather than waiting to be asked: applications, the repository holding the
  database schema or migrations, infrastructure and IaC, shared libraries and
  config repositories, and every ticketing board in use. The wording names what
  the assessment judges — how mature, how stable and how supportable the
  technology is — so the ask reads as audit quality rather than an inventory
  chore, and it claims no detection, because this client cannot see a target
  that was never registered. One `registry::COVERAGE_COACHING` constant, so the
  CLI and a later chat wizard present the same substance (#5822).
