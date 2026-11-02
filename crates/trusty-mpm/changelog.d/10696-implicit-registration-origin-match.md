Fixed
- Session auto-registration no longer adds a second project record for a repository that is already registered under another name. It now skips a session whose origin matches an existing record, including a legacy record with no stored project key, and an ssh origin matches the https URL of the same `owner/repo`.
