# api-design-patterns — REST, GraphQL and gRPC patterns

## REST API Patterns

### Resource Naming

✅ **Good: Plural nouns, hierarchical**
```
GET    /users              # List users
GET    /users/123          # Get user
POST   /users              # Create user
PUT    /users/123          # Update user (full)
PATCH  /users/123          # Update user (partial)
DELETE /users/123          # Delete user
GET    /users/123/orders   # User's orders (sub-resource)
```

❌ **Bad: Verbs, mixed conventions**
```
GET    /getUsers           # Don't use verbs
POST   /user/create        # Don't use verbs
GET    /Users/123          # Don't capitalize
GET    /user/123           # Don't mix singular/plural
```

### HTTP Status Codes

**Success Codes**:
- `200 OK`: Successful GET, PUT, PATCH, DELETE with body
- `201 Created`: Successful POST, return Location header
- `202 Accepted`: Async operation started
- `204 No Content`: Successful DELETE, no body

**Client Error Codes**:
- `400 Bad Request`: Invalid input, validation error
- `401 Unauthorized`: Missing or invalid authentication
- `403 Forbidden`: Authenticated but insufficient permissions
- `404 Not Found`: Resource doesn't exist
- `409 Conflict`: State conflict (duplicate, version mismatch)
- `422 Unprocessable Entity`: Semantic validation error
- `429 Too Many Requests`: Rate limit exceeded

**Server Error Codes**:
- `500 Internal Server Error`: Unexpected error
- `502 Bad Gateway`: Upstream service error
- `503 Service Unavailable`: Temporary outage
- `504 Gateway Timeout`: Upstream timeout

### Error Response Format

✅ **Consistent error structure**
```json
{
  "error": {
    "code": "VALIDATION_ERROR",
    "message": "Invalid request parameters",
    "details": [
      {
        "field": "email",
        "message": "Invalid email format",
        "code": "INVALID_FORMAT"
      }
    ],
    "request_id": "req_abc123",
    "documentation_url": "https://api.example.com/docs/errors/validation"
  }
}
```

### Pagination Patterns

**Offset Pagination** (simple, familiar):
```
GET /users?limit=20&offset=40
```
✅ Use for: Small datasets, admin interfaces
❌ Avoid for: Large datasets (skips become expensive), real-time data

**Cursor Pagination** (stable, efficient):
```
GET /users?limit=20&cursor=eyJpZCI6MTIzfQ
Response: { "data": [...], "next_cursor": "eyJpZCI6MTQzfQ" }
```
✅ Use for: Infinite scroll, real-time feeds, large datasets
❌ Avoid for: Random access, page numbers

**Keyset Pagination** (performant):
```
GET /users?limit=20&after_id=123
```
✅ Use for: Ordered data, database index friendly
❌ Avoid for: Complex sorting, multiple sort keys

See `rest-patterns.md` for filtering, sorting, field selection, HATEOAS

## GraphQL Patterns

### Schema Design

✅ **Good: Clear types, nullable by default**
```graphql
type User {
  id: ID!                    # Non-null ID
  email: String!             # Required field
  name: String               # Optional (nullable by default)
  createdAt: DateTime!
  orders: [Order!]!          # Non-null array of non-null orders
}

type Query {
  user(id: ID!): User
  users(first: Int, after: String): UserConnection!
}

type Mutation {
  createUser(input: CreateUserInput!): CreateUserPayload!
}

input CreateUserInput {
  email: String!
  name: String
}

type CreateUserPayload {
  user: User
  userEdge: UserEdge
  errors: [UserError!]
}
```

### Resolver Patterns

**Avoid N+1 Queries with DataLoader**:
```typescript
import DataLoader from 'dataloader';

const userLoader = new DataLoader(async (userIds: string[]) => {
  const users = await db.users.findMany({ where: { id: { in: userIds } } });
  return userIds.map(id => users.find(u => u.id === id));
});

// Resolver batches queries automatically
const resolvers = {
  Order: {
    user: (order) => userLoader.load(order.userId)
  }
};
```

### Query Complexity Analysis

Prevent expensive queries:
```typescript
import { createComplexityLimitRule } from 'graphql-validation-complexity';

const server = new ApolloServer({
  schema,
  validationRules: [
    createComplexityLimitRule(1000, {
      onCost: (cost) => console.log('Query cost:', cost),
    }),
  ],
});
```

See `graphql-patterns.md` for subscriptions, relay cursor connections, error handling

## gRPC Patterns

### Service Definition

```protobuf
syntax = "proto3";

package users.v1;

service UserService {
  rpc GetUser (GetUserRequest) returns (User) {}
  rpc ListUsers (ListUsersRequest) returns (ListUsersResponse) {}
  rpc CreateUser (CreateUserRequest) returns (User) {}
  rpc StreamUsers (StreamUsersRequest) returns (stream User) {}
  rpc BidiChat (stream ChatMessage) returns (stream ChatMessage) {}
}

message User {
  string id = 1;
  string email = 2;
  string name = 3;
  google.protobuf.Timestamp created_at = 4;
}

message GetUserRequest {
  string id = 1;
}

message ListUsersRequest {
  int32 page_size = 1;
  string page_token = 2;
}

message ListUsersResponse {
  repeated User users = 1;
  string next_page_token = 2;
}
```

### Error Handling

```go
import (
    "google.golang.org/grpc/codes"
    "google.golang.org/grpc/status"
)

func (s *server) GetUser(ctx context.Context, req *pb.GetUserRequest) (*pb.User, error) {
    if req.Id == "" {
        return nil, status.Error(codes.InvalidArgument, "user ID is required")
    }

    user, err := s.db.GetUser(ctx, req.Id)
    if err != nil {
        if errors.Is(err, sql.ErrNoRows) {
            return nil, status.Error(codes.NotFound, "user not found")
        }
        return nil, status.Error(codes.Internal, "database error")
    }

    return user, nil
}
```

See `grpc-patterns.md` for streaming, interceptors, metadata, health checks
