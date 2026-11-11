# Rung 2: put a Claude token on EVO (Bob's checklist)

Task ar-T14, issue #11054. The token never goes into a chat, a tmux pane, a command line or a log.
Do these steps on the Mac only after tm-mpm-utils-01 says go.

1. **Make the token.** Open a new window in the Terminal app on the Mac. Do not use a tmux pane or a Claude Code session. Run `claude setup-token`, then approve the request in the browser. The terminal prints the token. It is valid for one year and works for model requests only.

2. **Send it to EVO.** Copy the token. In the same window, paste this line and press Enter:

   ```sh
   read -rs T && printf 'CLAUDE_CODE_OAUTH_TOKEN=%s\n' "$T" | ssh evo-linux 'umask 077 && mkdir -p ~/cloudtest/tm-trusty-cloud-cloudtest/secrets && cat > ~/cloudtest/tm-trusty-cloud-cloudtest/secrets/claude.env && chmod 600 ~/cloudtest/tm-trusty-cloud-cloudtest/secrets/claude.env'; unset T
   ```

   The terminal now waits and shows nothing. Paste the token and press Enter. The token travels over ssh input only, so it is not in shell history, on any command line or in any log.

3. **Clean up the Mac.** Press Cmd-K to clear the window, then close it. Copy any other text, so the token is no longer on the clipboard.

4. **Check it, and say so.** Run `ssh evo-linux 'stat -c "%a %s" ~/cloudtest/tm-trusty-cloud-cloudtest/secrets/claude.env'`. It must print `600` and a size; never print the file itself. Then tell the Architect "token placed", without the value.

5. **Revoke it.** When rung 2 ends, tm-trusty-cloud deletes the file and both containers. The token itself stays valid until it expires after one year. As of 2026-11-11, Anthropic's docs give no user path to revoke a `setup-token` token. Ask Anthropic support to revoke it if you need it gone sooner.
