# Runbook: Move the Slack gateway tokens into the credential store

Move `SLACK_APP_TOKEN` and `SLACK_BOT_TOKEN` out of the trusty-agents Slack
gateway's LaunchAgent plist (`~/Library/LaunchAgents/com.trusty.agents.slack.plist`)
into the trusty-common credential store, then confirm the gateway still starts.

Tracked in [#10120](https://github.com/bobmatnyc/trusty-tools/issues/10120).
This runbook covers migration only. Token rotation and the choice of store are
a later decision.

## How the gateway finds its tokens

The gateway (`tagent --slack`) resolves each token from the first source that
holds a non-empty value:

1. The process environment, including the plist's `EnvironmentVariables`.
2. `.env.local`, found by searching upward from the gateway's working directory.
3. The trusty-common credential store, under the provider key the doctor fix
   writes: `slack-app` for `SLACK_APP_TOKEN`, `slack` for `SLACK_BOT_TOKEN`.

A token missing from all three stops the gateway with an error that names the
variable, for example ``no value configured for `SLACK_APP_TOKEN` in the
environment, `.env.local`, or the credential store``. No error or log line
carries a token value.

A value left in the plist or in `.env.local` still wins over the store. Remove
the token from both, or the store copy is never read.

## Preconditions

1. Install a trusty-agents build that contains #10120. An older gateway reads
   the environment only and stops after step 2 below.
2. Install `tm` (trusty-mpm) with `tm doctor --fix-launchd-secrets` (#8236).

## Steps

1. Preview the move. This writes nothing:

   ```bash
   tm doctor --fix-launchd-secrets
   ```

   The plist's line lists `SLACK_APP_TOKEN` and `SLACK_BOT_TOKEN` among the
   credentials it would migrate.

2. Apply it. This copies each registered credential into the store, strips it
   from the plist, and sets the plist to mode 0600:

   ```bash
   tm doctor --fix-launchd-secrets --yes
   ```

3. Check that the gateway's `.env.local`, if one exists at or above its working
   directory, does not also set either token. Delete the two lines if it does.

4. Reload the gateway so it starts without the tokens in its environment:

   ```bash
   launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/com.trusty.agents.slack.plist
   launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.trusty.agents.slack.plist
   ```

## Verify

1. The plist no longer holds the tokens. A second preview reports nothing left
   to migrate for this plist:

   ```bash
   tm doctor --fix-launchd-secrets
   ```

2. The gateway is running. Read the `state =` line, not a "not running" grep:

   ```bash
   launchctl print gui/$(id -u)/com.trusty.agents.slack | grep 'state ='
   ```

   Expect `state = running`.

3. The gateway log (the plist's `StandardErrorPath`) shows
   `Starting Slack bot in Socket Mode` after the reload, and no
   `no value configured for` line.

4. Send `/slack-status` in a paired channel and expect a reply.

## Roll back

If the gateway does not start, put the two tokens back in the plist's
`EnvironmentVariables` (or in `.env.local`) and reload it as in step 4. The
environment tier wins over the store, so the gateway starts again without
removing the store entries.
