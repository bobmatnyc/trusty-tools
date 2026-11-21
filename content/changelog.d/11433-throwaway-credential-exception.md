Changed
- The Architect and supervisor hard limits, `tm-secrets`, and the secret-handling lines in the Architect skills, `secrets-manager` and `local-ops` carry one exception (#11433): a credential the owner explicitly declares throwaway or demo may be stored and used in plaintext in a project file, marked `throwaway` at the storage site, without a vault. All other secrets keep the rule.
