Changed
- `tm serve --stdio` waits up to 20 minutes for a `sessions_chat` call instead of 60 s, so a long action-capable chat turn is not cut off. Every other request keeps the 60 s bound (#9979).
