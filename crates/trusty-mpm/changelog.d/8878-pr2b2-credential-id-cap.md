Fixed
- A passkey whose credential id is too long for a device record file name (over 250 base64url characters) is refused before its enrol ticket is spent, instead of spending the ticket and then failing the write (#8878).
