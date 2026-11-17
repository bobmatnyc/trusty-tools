Changed

- The `secrets-manager` agent no longer lists a copy to a sync target as refused: an agent may push a key with no deny, including a push-only key, and a denied key still fails with `agent_key_denied` (#10999).
