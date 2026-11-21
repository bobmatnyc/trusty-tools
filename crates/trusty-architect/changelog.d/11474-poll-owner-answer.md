Fixed

- The poll cycle (`tm architect poll`) raises one `owner_answer` alert per answered, cancelled or Architect-Do dashboard decision, fingerprinted `<id>@<at>` as `fleet-poll.py` does, re-emits it every 30 minutes while it stays pending, and raises `owner_answer_error` when the owner-queue store cannot be read. Before, an answer raised no alert.
