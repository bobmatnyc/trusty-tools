Fixed
- An agent that `extends` another now keeps its own `[agent].provider_id` pin, and inherits the base's pin when it declares none. Before, the merge dropped the child's pin, so `cto-assistant` ran on the ambient provider instead of Bedrock (#11043).
