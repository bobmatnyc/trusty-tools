Fixed

- `tm compress`'s native-chain tests force the native fallback instead of assuming the host has no `rtk`, so they pass identically with and without a Homebrew `rtk`; a new test covers the `rtk pipe` arm where a host has one ([#7325](https://github.com/bobmatnyc/trusty-tools/issues/7325)).
