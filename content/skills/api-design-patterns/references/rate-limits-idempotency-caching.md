# api-design-patterns — rate limiting, idempotency and caching

## Rate Limiting

### Token Bucket (Burst-Friendly)

```
Bucket: 100 tokens, refill 10/second
Request costs 1 token
Allows bursts up to bucket size
```

**Headers**:
```http
HTTP/1.1 200 OK
X-RateLimit-Limit: 100
X-RateLimit-Remaining: 73
X-RateLimit-Reset: 1640995200
```

**429 Response**:
```http
HTTP/1.1 429 Too Many Requests
Retry-After: 60
X-RateLimit-Limit: 100
X-RateLimit-Remaining: 0
X-RateLimit-Reset: 1640995200

{
  "error": {
    "code": "RATE_LIMIT_EXCEEDED",
    "message": "Rate limit exceeded. Try again in 60 seconds.",
    "limit": 100,
    "reset_at": "2025-01-01T00:00:00Z"
  }
}
```

### Sliding Window (Fair Distribution)

Counts requests in rolling time window. More accurate than fixed window.

### Per-User vs Per-IP

- **Per-User**: Authenticated requests, fair quotas
- **Per-IP**: Unauthenticated requests, prevent abuse
- **Combined**: Both limits, take stricter

## Idempotency

### Idempotent Methods (HTTP Spec)

**Naturally Idempotent**: GET, PUT, DELETE, HEAD, OPTIONS
**Not Idempotent**: POST, PATCH

### Idempotency Keys

Make POST requests idempotent:
```http
POST /v1/payments
Idempotency-Key: uuid-or-client-generated-key
Content-Type: application/json

{
  "amount": 1000,
  "currency": "USD",
  "customer": "cust_123"
}
```

**Server behavior**:
1. First request: Process and store result with key
2. Duplicate request (same key): Return stored result (200 or 201)
3. Different request (same key): Return 409 Conflict

**Implementation**:
```typescript
const idempotencyKey = req.headers['idempotency-key'];
if (idempotencyKey) {
  const cached = await redis.get(`idempotency:${idempotencyKey}`);
  if (cached) {
    return res.status(cached.status).json(cached.body);
  }
}

const result = await processPayment(req.body);
await redis.setex(`idempotency:${idempotencyKey}`, 86400, {
  status: 201,
  body: result
});
```

### Conditional Requests

Use ETags for safe updates:
```http
# Get resource with ETag
GET /v1/users/123
Response: ETag: "abc123"

# Update only if unchanged
PUT /v1/users/123
If-Match: "abc123"

# 412 Precondition Failed if ETag changed
```

## Caching Strategies

### HTTP Caching Headers

```http
# Public, cacheable for 1 hour
Cache-Control: public, max-age=3600

# Private (user-specific), revalidate
Cache-Control: private, must-revalidate, max-age=0

# No caching
Cache-Control: no-store, no-cache, must-revalidate
```

### ETag Validation

```http
# Server returns ETag
GET /v1/users/123
Response:
  ETag: "33a64df551425fcc55e4d42a148795d9f25f89d4"
  Cache-Control: max-age=3600

# Client conditional request
GET /v1/users/123
If-None-Match: "33a64df551425fcc55e4d42a148795d9f25f89d4"

# 304 Not Modified if unchanged (saves bandwidth)
HTTP/1.1 304 Not Modified
```

### Last-Modified

```http
GET /v1/users/123
Response:
  Last-Modified: Wed, 21 Oct 2025 07:28:00 GMT

# Conditional request
GET /v1/users/123
If-Modified-Since: Wed, 21 Oct 2025 07:28:00 GMT

# 304 Not Modified if not modified
```
