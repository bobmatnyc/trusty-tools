Fixed
- connect-retry and stale-socket tests hold their socket instead of dropping and rebinding, removing a console test flake (Refs #3674, #10323).
