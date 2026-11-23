Changed
- A chat turn with attachments builds its `trusty_memory` `ChatMessage` with `ChatMessage::new(..).with_attachments(..)` instead of a struct literal, so the build keeps compiling once trusty-memory marks it `#[non_exhaustive]`. The stored turn is unchanged ([#9278](https://github.com/bobmatnyc/trusty-tools/issues/9278))
