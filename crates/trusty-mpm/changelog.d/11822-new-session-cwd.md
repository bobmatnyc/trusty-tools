Fixed
- The `tm` / `tm ls` new-session dialog (`n`) now offers and preselects the repo you ran `tm` in, registering it on Enter when the registry does not hold it; a cwd that is no repo, or a temp checkout, keeps the old preselect (#11822).
- Each row in that dialog now says why it is offered: `cwd` or `registered` (#11822).
- The arrow keys now work in the dialog's free-text step: they return to the project list and move the highlight instead of doing nothing (#11822).
