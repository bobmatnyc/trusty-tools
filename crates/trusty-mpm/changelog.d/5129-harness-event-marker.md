Fixed

- The assembled session-manager prompt told the SM to watch tcode for
  `__HARNESS_EVENT__`; tcode emits `__OMPM_EVENT__`. The harness-understanding
  sections now carry the emitted marker, and the prompt tests assert against
  `trusty_agents_common::events::EVENT_LINE_PREFIX` instead of a literal — they
  previously passed by agreeing with the wrong instruction text
  ([#5129](https://github.com/bobmatnyc/trusty-tools/issues/5129)).
