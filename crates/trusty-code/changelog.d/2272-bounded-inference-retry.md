Fixed

- Retry transient inference failures inside the current assistant turn with a
  fixed three-attempt budget, while preventing retries after streamed text is
  visible and keeping the existing run deadline and re-delegation boundary.
