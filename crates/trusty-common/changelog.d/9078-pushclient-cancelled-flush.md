Fixed
- `control_bus::PushClient::flush` no longer loses a frame when the flush future is dropped mid-write (a flush task aborted at shutdown, or an outer timeout): the frame goes back to the front of the buffer, as it already did on a write error or the per-frame timeout (#9078).
