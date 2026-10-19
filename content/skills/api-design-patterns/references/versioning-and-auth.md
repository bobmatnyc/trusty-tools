# api-design-patterns — versioning and authentication

## Versioning Strategies

### URI Versioning (Simple, Explicit)

✅ **Most common, easy to understand**
```
GET /v1/users/123
GET /v2/users/123
```

**Pros**: Clear, easy to route, browser-friendly
**Cons**: Couples version to URL, duplicates routes

### Header Versioning (Clean URLs)

```
GET /users/123
Accept: application/vnd.myapi.v2+json
```

**Pros**: Clean URLs, version separate from resource
**Cons**: Less visible, harder to test manually

### Content Negotiation (Granular)

```
GET /users/123
Accept: application/vnd.myapi.user.v2+json
```

**Pros**: Resource-level versioning, backward compatible
**Cons**: Complex, harder to implement

### Version Deprecation Process

```json
{
  "version": "1.0",
  "deprecated": true,
  "sunset_date": "2025-12-31",
  "migration_guide": "https://docs.api.com/v1-to-v2",
  "replacement_version": "2.0"
}
```

**Include deprecation warnings**:
```
HTTP/1.1 200 OK
Deprecation: true
Sunset: Sat, 31 Dec 2025 23:59:59 GMT
Link: <https://docs.api.com/v1-to-v2>; rel="deprecation"
```

See `versioning-strategies.md` for detailed migration patterns

## Authentication & Authorization

### OAuth 2.0 (Delegated Access)

**Use for**: Third-party access, user consent, token refresh

**Authorization Code Flow** (most secure for web/mobile):
```
1. Client redirects to /authorize
2. User authenticates, grants permissions
3. Auth server redirects to callback with code
4. Client exchanges code for access token
5. Client uses access token for API requests
```

```http
# Request token
POST /oauth/token
Content-Type: application/x-www-form-urlencoded

grant_type=authorization_code
&code=AUTH_CODE
&redirect_uri=https://client.com/callback
&client_id=CLIENT_ID
&client_secret=CLIENT_SECRET

# Response
{
  "access_token": "eyJhbGc...",
  "token_type": "Bearer",
  "expires_in": 3600,
  "refresh_token": "tGzv3JOkF0XG5Qx2TlKWIA",
  "scope": "read write"
}

# Use token
GET /v1/users/me
Authorization: Bearer eyJhbGc...
```

### JWT (Stateless Auth)

**Use for**: Microservices, stateless API auth, short-lived tokens

✅ **Good: Minimal claims, short expiry**
```json
{
  "sub": "user_123",
  "iat": 1516239022,
  "exp": 1516242622,
  "scope": "read:users write:orders"
}
```

**Validation**:
```typescript
import jwt from 'jsonwebtoken';

const token = req.headers.authorization?.split(' ')[1];
const payload = jwt.verify(token, process.env.JWT_SECRET);
req.userId = payload.sub;
```

### API Keys (Service-to-Service)

**Use for**: Server-to-server, CLI tools, webhooks

```http
GET /v1/users
X-API-Key: sk_live_abc123...

# Or query parameter (less secure)
GET /v1/users?api_key=sk_live_abc123
```

**Key Practices**:
- Prefix keys with environment (`sk_live_`, `sk_test_`)
- Hash keys before storage (bcrypt, scrypt)
- Allow key rotation without downtime
- Support multiple keys per user
- Rate limit per key

See `authentication.md` for API key rotation, scopes, RBAC
