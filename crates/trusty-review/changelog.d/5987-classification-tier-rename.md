Changed

- The verifier and summarizer roles request `ModelTier::Classification`, renamed from `ModelTier::Haiku` in trusty-common. Both roles resolve the same model as before; a doc comment at the role mapping records that they select that tier for cost, not because verifying or summarising is classification. See #5987.
