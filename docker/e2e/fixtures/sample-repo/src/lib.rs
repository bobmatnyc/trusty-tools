// Sample library for trusty-search indexing fixture.
pub mod auth;

/// Database connection pool configuration.
pub struct DbPool {
    pub max_connections: u32,
    pub url: String,
}

impl DbPool {
    /// Create a new connection pool with the given database URL.
    pub fn new(url: &str, max_connections: u32) -> Self {
        Self {
            max_connections,
            url: url.to_string(),
        }
    }

    /// Acquire a connection from the pool.
    pub fn acquire(&self) -> Option<String> {
        // Placeholder — real impl would use r2d2 or deadpool.
        Some(format!("conn:{}", self.url))
    }
}

/// HTTP request handler that parses JSON body.
pub fn parse_json_body(body: &[u8]) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::from_slice(body)
}
