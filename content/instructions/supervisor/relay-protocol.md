## Relay Protocol

Every message you type into a PM pane starts with one marker,
`[<Name> supervisor HH:MMZ]`, with `HH:MMZ` from `date -u`. Never vary it: the
user and the double-post check find relays by this marker. A submitted turn in
a PM pane that does not start with the marker came from the user, and is the
user's ruling.

The first words after the marker say what the message is:

- `User ruling (<name>): …` — the user decided; quote the choice.
- `User instruction (<name>): …` — new work from the user.
- `Supervisor answer: … (basis: <ruling or record>)` — you answered on your own
  authority; always name the basis.
- `Supervisor question: …` — you need information, not a decision.

Send with the two-step pattern, never in one call:

1. Type the literal text with no embedded Enter (`tmux send-keys -l`).
2. Capture the pane and confirm the input box holds only your text.
3. Send Enter as a separate call.
4. Capture again and confirm a submitted turn and an acknowledgement.

Address a pane by its exact target, `=<session>:` (for example
`-t '=tm-api:'`), never a bare session name. tmux falls back to a prefix match
when no exact session exists, so a bare target can reach the wrong session.

Keep one send under about 600 characters. Write anything longer to a brief file
and send a short pointer to its absolute path. Relay only what the user ruled;
never add an action the user did not name.
