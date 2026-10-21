---
name: api-design-patterns
description: "Comprehensive API design patterns covering REST, GraphQL, gRPC, versioning, authentication, and modern API best practices"
user-invocable: false
metadata:
  version: "1.1.0"
category: agent-reference
effort: high
---
# API Design Patterns

Design robust, scalable APIs using proven patterns for REST, GraphQL, and gRPC with proper versioning, authentication, and error handling.

## Quick Reference

**API Style Selection**:
- REST: Resource-based CRUD, simple clients, HTTP-native caching
- GraphQL: Client-driven queries, complex data graphs, real-time subscriptions
- gRPC: High-performance RPC, microservices, strong typing, streaming

**Critical Patterns**:
- Versioning: URI (`/v1/users`), header (`Accept: application/vnd.api+json;version=1`), content negotiation
- Pagination: Offset (simple), cursor (stable), keyset (performant)
- Auth: OAuth2 (delegated), JWT (stateless), API keys (service-to-service)
- Rate limiting: Token bucket, fixed window, sliding window
- Idempotency: Idempotency keys, conditional requests, safe retry

**See references/ for deep dives**: `rest-patterns.md`, `graphql-patterns.md`, `grpc-patterns.md`, `versioning-strategies.md`, `authentication.md`

## Where Each Task Lives

This file holds what every API design task needs. Everything else sits in one
file per task under `references/`, beside this file. Read the file your task
needs, and only that file. A pointer elsewhere that names a section of
`api-design-patterns` resolves here: find the section name in the middle column.

| Task | Sections | File |
|---|---|---|
| Name REST resources, choose status codes and the error format, paginate; design a GraphQL schema or a gRPC service | "REST API Patterns", "GraphQL Patterns", "gRPC Patterns" | [references/style-patterns.md](references/style-patterns.md) |
| Version an API and deprecate a version; choose OAuth 2.0, JWT or API keys | "Versioning Strategies", "Authentication & Authorization" | [references/versioning-and-auth.md](references/versioning-and-auth.md) |
| Rate-limit an API, make a write safe to retry, set caching headers | "Rate Limiting", "Idempotency", "Caching Strategies" | [references/rate-limits-idempotency-caching.md](references/rate-limits-idempotency-caching.md) |
| Deliver and sign webhooks; document an API; contract-test or load-test it | "Webhooks", "API Documentation", "Testing Strategies" | [references/webhooks-docs-testing.md](references/webhooks-docs-testing.md) |
| Go deeper on REST | "Resource Modeling", "HTTP Methods Deep Dive", "Query Parameters", "Pagination Patterns", "HATEOAS (Hypermedia)", "Batch Operations", "Async Operations", "Compression", "Content Negotiation", "Conditional Requests", "Performance Optimization", "Security Headers", "CORS (Cross-Origin Resource Sharing)", "REST API Testing", "Best Practices Summary", "Additional Resources" | [references/rest-patterns.md](references/rest-patterns.md) |
| Go deeper on GraphQL | "Schema Design Best Practices", "Resolver Patterns", "Pagination Patterns", "Mutations", "Subscriptions (Real-Time)", "Directives", "Performance Optimization", "Schema Stitching and Federation", "Testing", "Best Practices Summary", "Additional Resources" | [references/graphql-patterns.md](references/graphql-patterns.md) |
| Go deeper on gRPC | "Protocol Buffers (Protobuf)", "Service Definition", "Server Implementation", "Client Implementation", "Error Handling", "Interceptors (Middleware)", "Metadata (Headers)", "Performance Optimization", "TLS/SSL Security", "Health Checking", "Testing", "Best Practices Summary", "Additional Resources" | [references/grpc-patterns.md](references/grpc-patterns.md) |
| Go deeper on versioning and migration | "Why Version APIs?", "URI Versioning", "Header Versioning", "Content Negotiation", "Query Parameter Versioning", "Version Negotiation", "Deprecation Strategy", "Migration Patterns", "GraphQL Versioning", "gRPC Versioning", "Version Compatibility Matrix", "Testing Multiple Versions", "Best Practices Summary", "Decision Tree", "Additional Resources" | [references/versioning-strategies.md](references/versioning-strategies.md) |
| Go deeper on OAuth, JWT, API keys and RBAC | "Authentication vs Authorization", "OAuth 2.0", "JWT (JSON Web Tokens)", "API Keys", "Role-Based Access Control (RBAC)", "Security Best Practices", "Best Practices Summary", "Additional Resources" | [references/authentication.md](references/authentication.md) |

## Core Principles

### Universal API Design Standards

Apply these principles across all API styles:

**1. Consistency Over Cleverness**
- Follow established conventions for your API style
- Use predictable naming patterns (snake_case or camelCase, pick one)
- Maintain consistent error response formats
- Version breaking changes, never surprise clients

**2. Design for Evolution**
- Plan for versioning from day one
- Use optional fields with sensible defaults
- Deprecate gracefully with sunset dates
- Document breaking vs non-breaking changes

**3. Security by Default**
- Require authentication unless explicitly public
- Use HTTPS/TLS for all production endpoints
- Implement rate limiting and throttling
- Validate and sanitize all inputs
- Return minimal error details to clients

**4. Developer Experience First**
- Provide comprehensive documentation (OpenAPI, GraphQL schema)
- Return meaningful error messages with actionable guidance
- Use standard HTTP status codes correctly
- Include request IDs for debugging
- Offer SDKs and code generators

## API Style Decision Tree

### When to Choose REST

✅ **Use REST when:**
- Building CRUD-focused resource APIs
- Clients need HTTP caching (ETags, Cache-Control)
- Wide platform compatibility required (browsers, mobile, IoT)
- Simple, stateless client-server model fits
- Team familiar with HTTP/REST conventions

❌ **Avoid REST when:**
- Complex data fetching with nested relationships (N+1 queries)
- Real-time updates are primary use case
- Need strong typing and code generation
- High-performance RPC between microservices

**Example Use Cases**: Public APIs, mobile backends, traditional web services

### When to Choose GraphQL

✅ **Use GraphQL when:**
- Clients need flexible, client-driven queries
- Complex data graphs with nested relationships
- Multiple client types with different data needs
- Real-time subscriptions required
- Strong typing and schema validation needed

❌ **Avoid GraphQL when:**
- Simple CRUD operations dominate
- HTTP caching is critical (GraphQL uses POST)
- File uploads are primary feature (requires extensions)
- Team lacks GraphQL expertise
- Performance optimization is complex (N+1 problem)

**Example Use Cases**: Client-facing APIs, dashboards, mobile apps with varied UIs

### When to Choose gRPC

✅ **Use gRPC when:**
- Microservice-to-microservice communication
- High performance and low latency critical
- Bidirectional streaming needed
- Strong typing with Protocol Buffers
- Polyglot environments (language interop)

❌ **Avoid gRPC when:**
- Browser clients (limited support, needs grpc-web)
- HTTP/JSON required for compatibility
- Human-readable payloads preferred
- Simple request/response patterns

**Example Use Cases**: Internal microservices, streaming data, service mesh

## Anti-Patterns

❌ **Over-fetching (REST)**: Returning entire objects when fields are unused
✅ **Solution**: Support field selection (`?fields=id,name,email`)

❌ **Under-fetching (REST)**: Requiring multiple requests for related data
✅ **Solution**: Support expansion (`?expand=orders,profile`) or use GraphQL

❌ **Chatty APIs**: Too many round-trips for common operations
✅ **Solution**: Batch endpoints, compound documents, or GraphQL

❌ **Ignoring HTTP semantics**: Using GET for mutations, wrong status codes
✅ **Solution**: Follow HTTP spec, use correct methods and status codes

❌ **Exposing internal structure**: URLs/schemas mirror database
✅ **Solution**: Design resource-oriented APIs independent of storage

❌ **Missing versioning**: Breaking changes without version increments
✅ **Solution**: Version from day one, never break existing versions

❌ **Poor error messages**: Generic "An error occurred"
✅ **Solution**: Specific, actionable error messages with codes

❌ **No rate limiting**: APIs vulnerable to abuse
✅ **Solution**: Implement rate limiting from the start

## Related Skills

- **graphql**: Deep GraphQL schema design, resolvers, Apollo Server
- **typescript**: Type-safe API clients and servers
- **nodejs-backend**: Express/Fastify REST API implementation
- **django**: Django REST Framework patterns
- **fastapi**: FastAPI Python REST/GraphQL APIs
- **flask**: Flask-RESTful patterns

## References

- **rest-patterns.md**: Deep REST coverage (HATEOAS, filtering, field selection)
- **graphql-patterns.md**: GraphQL subscriptions, relay cursor connections, federation
- **grpc-patterns.md**: Streaming patterns, interceptors, service mesh integration
- **versioning-strategies.md**: Detailed versioning approaches and migration patterns
- **authentication.md**: OAuth flows, JWT best practices, API key rotation, RBAC

## Additional Resources

- [REST API Design Rulebook](https://www.oreilly.com/library/view/rest-api-design/9781449317904/) - O'Reilly REST guide
- [GraphQL Best Practices](https://graphql.org/learn/best-practices/) - Official GraphQL guide
- [gRPC Best Practices](https://grpc.io/docs/guides/performance/) - Official gRPC guide
- [RFC 7807: Problem Details for HTTP APIs](https://tools.ietf.org/html/rfc7807) - Standard error format
- [OpenAPI Specification](https://spec.openapis.org/oas/latest.html) - REST documentation standard
