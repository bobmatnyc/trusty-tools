Fixed
- SM provider calls are priced at current Anthropic and OpenAI list prices, taken from the `llm-prices` crate (each row records its source page and the date it was checked). Models the table doesn't list fall back to the existing family prices, so pricing never fails open to $0.
