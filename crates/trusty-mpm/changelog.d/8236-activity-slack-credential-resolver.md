Fixed
- The managed-session activity route surfaces `classification` when `OPENROUTER_API_KEY` lives in the credential store rather than the daemon's environment (#8236). It read only the process environment, so moving the key out of the LaunchAgent plist hid the field after a restart.
- `tm slack start` resolves `SLACK_BOT_TOKEN` / `SLACK_APP_TOKEN` through the shared credential resolver (process env, `.env.local`, then the credential store), like the Telegram bot (#8236). `.env` is no longer read; move a token kept there into `.env.local` or the store.
