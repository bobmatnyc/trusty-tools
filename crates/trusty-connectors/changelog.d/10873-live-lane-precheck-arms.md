Fixed
- The live lane (`tests/live_lane.rs`) stops at once, naming `OKF_LIVE_DRIVE_FOLDER`, when that variable is set but empty; before, the lane ran the pre-check and the mail imports and failed late in the Docs outcomes. New offline tests cover the pre-check's transport-error and tokeninfo 403/404 arms (#10873).
