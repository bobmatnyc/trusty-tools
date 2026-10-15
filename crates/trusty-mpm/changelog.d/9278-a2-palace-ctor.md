Changed
- The Session-Manager and portfolio manager palaces are built with `trusty_memory::engine::palace::Palace::new(..).with_description(..)` instead of a struct literal, so the build keeps compiling once trusty-memory marks `Palace` `#[non_exhaustive]`. The palace written to disk is unchanged ([#9278](https://github.com/bobmatnyc/trusty-tools/issues/9278))
