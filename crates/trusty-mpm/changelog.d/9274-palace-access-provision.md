Breaking
- `daemon::manager::ManagerState::provision` and `daemon::manager::PortfolioPalace::provision` take a second argument, `access: PalaceAccess` (#9274): `Writer` for the daemon, which owns the palaces, and `ReadOnly` for `tm sm serve --stdio`. Pass the access that matches the process.
