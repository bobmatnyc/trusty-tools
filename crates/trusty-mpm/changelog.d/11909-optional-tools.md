Fixed

- `tm install` no longer runs `cargo install trusty-computer`, which is not published to crates.io. It reports trusty-computer as optional and still registers a copy you built yourself. `tm doctor` lists an absent trusty-computer as an info line with how to get it, not a warning (#11909).
