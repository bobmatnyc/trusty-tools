Fixed
- A managed session whose `origin` remote git cannot read now gets the nobody-token for gh. Before, it spawned with no gh pin and ran gh as the machine's active account (#8934).
