# Use the computer and apps

> Source: https://docs.x.ai/grok-bot/computer-and-apps  
> Also cross-checked: https://cursor.com/docs/grok-bot/work (The computer and apps)  
> Fetched: 2026-09-05

---

# Use the computer and apps

Grok Bot works from a persistent cloud computer. It can use a browser, command
line, files, and connected tools without depending on your laptop remaining
open.

## One computer, shared by all your Bots

Every Bot on your account uses the same computer:

- Browser cookies and signed-in sessions are shared
- Files are visible to every Bot
- Command-line credentials are shared
- One Bot can continue from work another Bot saved

The computer is assigned to your user account, not an individual Bot. Do not
place a credential or file on it if another Bot on your account should not be
able to use it.

Each Bot gets its own screen on the shared computer. Several Bots can therefore
use browser and desktop tools in parallel, although one Bot can run only one
computer-use task on its screen at a time. The screens are separate work
surfaces, not separate security boundaries.

## Watch computer work

Open **Agent Computer** from a conversation to view the shared desktop. The
preview shows clicks, typing, navigation, and current status.

You can leave the preview while work continues. Closing the Grok Bot app or
your laptop does not stop cloud work.

## Take over for a sensitive step

The Bot may ask you to take over for:

- A password or passkey
- Two-factor authentication
- A CAPTCHA
- A payment or identity check
- A site that explicitly requires a human

Open the computer, take control, complete only the blocked step, and tell the
Bot to continue. Avoid pasting passwords or one-time codes into chat.

For a supported connection that presents a secure secret request, enter the
value there instead. The value is masked and is not added to the conversation.

## Sign in once

Browser sessions persist so you usually do not need to sign in for each task.
Because the browser is shared, signing in for one Bot makes the session
available to your other Bots.

Some websites expire sessions, enforce short timeouts, or request verification
again. Ask the Bot to pause and notify you rather than attempting to bypass the
check.

## Connect an app

Connectors give a Bot a structured way to work with supported services.
Connectors are shown as **Plugins** in the current app.

1. Open Settings → Plugins.
2. Browse the available connectors.
3. Choose Add.
4. Complete authentication in your browser if requested.
5. In chat, type `@` to attach the connector to the task. Type `/` to reference
   a saved skill.

Prefer a connector when one is available: it is often more reliable than
clicking through a website. Use the browser for services without a connector or
for visual workflows a connector does not expose.

Installed connectors are account-wide. Their availability is not isolated to
one Bot.

Plugin OAuth tokens stay on Cursor's connector backend; Bots invoke tools
without receiving tokens (per Cursor security / work docs).

## Work with files

The computer has a shared workspace at `/workspace`. Ask Bots to keep durable
project files there and use clear project folders.

Files, browser state, and supported sign-ins are designed to survive normal
computer updates and recovery. Treat temporary directories, manually installed
packages, and uncommitted application state as replaceable.

## Update, recover, or reset the computer

- **Update Agent Computer** — rebuilds with the latest image while preserving durable state.
- **Recover Agent Computer** — replaces an unreachable computer while preserving durable state when offered.
- **Reset Agent Computer** — returns to the most recent durable snapshot; can discard recent unsaved work.

Conversations are stored outside the computer and survive even a reset.

## Your local computer is separate

The Grok Bot cloud computer is separate from the Mac or Windows computer in
front of you. A Bot only runs commands on your local computer when that
capability is enabled and you approve it under your local-computer policy
(ask every time / always allow / never). Default is ask every time.
