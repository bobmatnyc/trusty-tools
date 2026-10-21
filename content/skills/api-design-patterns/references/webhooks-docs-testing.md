# api-design-patterns — webhooks, documentation and testing

## Webhooks

### Event Delivery

```http
POST https://client.com/webhooks/payments
Content-Type: application/json
X-Webhook-Signature: sha256=abc123...
X-Webhook-Id: evt_abc123
X-Webhook-Timestamp: 1640995200

{
  "id": "evt_abc123",
  "type": "payment.succeeded",
  "created": 1640995200,
  "data": {
    "object": {
      "id": "pay_123",
      "amount": 1000,
      "status": "succeeded"
    }
  }
}
```

### Signature Verification

```typescript
import crypto from 'crypto';

function verifyWebhookSignature(
  payload: string,
  signature: string,
  secret: string
): boolean {
  const expectedSignature = crypto
    .createHmac('sha256', secret)
    .update(payload)
    .digest('hex');
  return crypto.timingSafeEqual(
    Buffer.from(signature),
    Buffer.from(`sha256=${expectedSignature}`)
  );
}
```

### Retry Strategy

- **Exponential backoff**: 1s, 2s, 4s, 8s, 16s, 32s, 64s
- **Timeout**: 5-30 seconds per attempt
- **Max attempts**: 3-7 attempts
- **Dead letter queue**: Store failed events
- **Manual retry**: UI for re-sending failed events

## API Documentation

### OpenAPI/Swagger (REST)

```yaml
openapi: 3.0.0
info:
  title: User API
  version: 1.0.0
paths:
  /users/{id}:
    get:
      summary: Get user by ID
      parameters:
        - name: id
          in: path
          required: true
          schema:
            type: string
      responses:
        '200':
          description: Successful response
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/User'
        '404':
          description: User not found
components:
  schemas:
    User:
      type: object
      required: [id, email]
      properties:
        id:
          type: string
        email:
          type: string
          format: email
        name:
          type: string
```

### GraphQL Schema (Self-Documenting)

GraphQL introspection provides automatic documentation. Use descriptions:

```graphql
"""
Represents a user account in the system.
Created via the createUser mutation.
"""
type User {
  """Unique identifier for the user"""
  id: ID!

  """Email address, must be unique"""
  email: String!

  """Optional display name"""
  name: String
}
```

### API Documentation Best Practices

1. **Interactive examples**: Provide working code samples
2. **Authentication guide**: Step-by-step auth setup
3. **Error catalog**: Document all error codes with examples
4. **Rate limits**: Clearly state limits and headers
5. **Changelog**: Track breaking and non-breaking changes
6. **Migration guides**: Version upgrade instructions
7. **SDKs**: Provide client libraries for popular languages

## Testing Strategies

### Contract Testing

```typescript
// Pact contract test
import { PactV3 } from '@pact-foundation/pact';

const provider = new PactV3({
  consumer: 'FrontendApp',
  provider: 'UserAPI'
});

it('gets a user by ID', () => {
  provider
    .given('user 123 exists')
    .uponReceiving('a request for user 123')
    .withRequest({
      method: 'GET',
      path: '/users/123'
    })
    .willRespondWith({
      status: 200,
      body: { id: '123', email: 'user@example.com' }
    });
});
```

### Load Testing

```javascript
// k6 load test
import http from 'k6/http';
import { check } from 'k6';

export const options = {
  stages: [
    { duration: '30s', target: 20 },
    { duration: '1m', target: 20 },
    { duration: '10s', target: 0 }
  ],
  thresholds: {
    http_req_duration: ['p(95)<500'], // 95% under 500ms
    http_req_failed: ['rate<0.01']    // <1% errors
  }
};

export default function () {
  const res = http.get('https://api.example.com/users');
  check(res, {
    'status is 200': (r) => r.status === 200,
    'response time < 500ms': (r) => r.timings.duration < 500
  });
}
```
