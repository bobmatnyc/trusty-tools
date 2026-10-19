Added
- `control_bus::SubscribeParams::since_id`: the id of the event a reader holds at `since_seq`. The bus refuses a resume from a non-zero `since_seq` without it, or when the event now at that `seq` has a different id, so a `seq` reassigned after a crash cannot make a reader skip events silently. Omitted from the frame when absent (#9075).
