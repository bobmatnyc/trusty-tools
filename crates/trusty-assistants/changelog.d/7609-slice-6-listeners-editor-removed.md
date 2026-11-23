Removed
- The agent configuration panel's **Listeners** section and its editor are gone. Both read and wrote `GET`/`PUT /api/agents/{name}/listeners`, deprecated in slice 5 and removed in slice 7. Everything they configured is now a channel: per-assistant bindings in the Channels view's Assistant scope, host-wide sources in its Global scope.
