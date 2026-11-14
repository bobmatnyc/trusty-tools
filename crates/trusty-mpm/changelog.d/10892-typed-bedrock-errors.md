Changed
- The SM Bedrock provider classifies a missing credential, a timeout and a transport fault from the shared adapter's typed `BedrockError` variant instead of the message text; the resulting `SmLlmError` classes are unchanged (#10892).
