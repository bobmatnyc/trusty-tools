//! Cloud-platform, observability, datastore, messaging, and networking rules.

use crate::classify::rules::types::Rule;

/// Why: cloud-provider commits (AWS / GCP / Azure) cluster naturally as a
/// "cloud" category for organisations doing infrastructure work, distinct
/// from generic build/CI churn.
/// What: returns three rules, one per provider, with cloud-service keywords
/// (S3 / EC2 / Lambda for AWS, BigQuery / Cloud Run for GCP, etc.).
/// Test: smoke-tested via the broad corpus in `corpus_uncategorized_below_1_percent`.
pub(super) fn cloud_platform_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "kw-cloud-aws".into(),
            category: "cloud".into(),
            subcategory: Some("aws".into()),
            keywords: vec![
                "aws ".into(),
                " aws".into(),
                "amazon web services".into(),
                "cloudformation".into(),
                "cloudfront".into(),
                "cloudwatch".into(),
                " s3 ".into(),
                "s3 bucket".into(),
                " ec2".into(),
                " lambda".into(),
                "aws lambda".into(),
                " eks".into(),
                " ecs".into(),
                " rds".into(),
                " sqs".into(),
                " sns".into(),
                "iam role".into(),
                "iam policy".into(),
                "route53".into(),
                "route 53".into(),
                "dynamodb".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.85,
        },
        Rule {
            id: "kw-cloud-gcp".into(),
            category: "cloud".into(),
            subcategory: Some("gcp".into()),
            keywords: vec![
                " gcp".into(),
                "google cloud".into(),
                "bigquery".into(),
                "cloud run".into(),
                "cloud functions".into(),
                " gke".into(),
                "pub/sub".into(),
                "pubsub".into(),
                "gcs bucket".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.85,
        },
        Rule {
            id: "kw-cloud-azure".into(),
            category: "cloud".into(),
            subcategory: Some("azure".into()),
            keywords: vec![
                " azure".into(),
                "azure functions".into(),
                "azure devops".into(),
                " aks".into(),
                "app service".into(),
                "blob storage".into(),
                "cosmos db".into(),
                "cosmosdb".into(),
            ],
            patterns: vec![],
            priority: 70,
            confidence: 0.85,
        },
    ]
}

/// Why: monitoring / observability commits (Datadog dashboards, Prometheus
/// metrics, Sentry alerts) are a distinct slice of platform work worth
/// reporting separately from features.
/// What: returns a single rule with the major SaaS vendors and OSS tools
/// (OpenTelemetry, ELK stack, etc.).
/// Test: covered indirectly by `corpus_uncategorized_below_1_percent`.
pub(super) fn observability_rules() -> Vec<Rule> {
    vec![Rule {
        id: "kw-monitoring".into(),
        category: "monitoring".into(),
        subcategory: None,
        keywords: vec![
            "datadog".into(),
            "prometheus".into(),
            "grafana".into(),
            "sentry".into(),
            "newrelic".into(),
            "new relic".into(),
            "pagerduty".into(),
            "splunk".into(),
            "opentelemetry".into(),
            "otel".into(),
            "tracing".into(),
            "metrics".into(),
            "alerting".into(),
            "alert rule".into(),
            "dashboard".into(),
            "log aggregation".into(),
            "elk stack".into(),
            "kibana".into(),
            "logstash".into(),
        ],
        patterns: vec![],
        priority: 65,
        confidence: 0.8,
    }]
}

/// Why: database-engine commits cluster naturally as their own category,
/// useful for organisations tracking data-platform work.
/// What: returns a single rule listing the major relational, key-value, and
/// document databases plus schema-migration markers.
/// Test: covered indirectly by the corpus smoke test.
pub(super) fn datastore_rules() -> Vec<Rule> {
    vec![Rule {
        id: "kw-database".into(),
        category: "database".into(),
        subcategory: None,
        keywords: vec![
            "postgresql".into(),
            "postgres".into(),
            " mysql".into(),
            "mariadb".into(),
            "sqlite".into(),
            " redis".into(),
            "mongodb".into(),
            "mongo db".into(),
            "elasticsearch".into(),
            "cassandra".into(),
            "dynamodb".into(),
            "schema change".into(),
            "schema migration".into(),
            "db schema".into(),
            "database schema".into(),
            "index migration".into(),
        ],
        patterns: vec![],
        priority: 65,
        confidence: 0.8,
    }]
}

/// Why: message-bus and queueing work (Kafka, RabbitMQ, NATS, SQS) is a
/// distinct platform slice.
/// What: returns a single rule covering the major brokers and queue
/// abstractions.
/// Test: smoke-covered by the broad corpus.
pub(super) fn messaging_rules() -> Vec<Rule> {
    vec![Rule {
        id: "kw-messaging".into(),
        category: "messaging".into(),
        subcategory: None,
        keywords: vec![
            " kafka".into(),
            "rabbitmq".into(),
            "rabbit mq".into(),
            " sqs".into(),
            " sns".into(),
            "pub/sub".into(),
            "pubsub".into(),
            "nats".into(),
            "amqp".into(),
            "message queue".into(),
            "event bus".into(),
            "event stream".into(),
        ],
        patterns: vec![],
        priority: 65,
        confidence: 0.8,
    }]
}

/// Why: networking / web-infra commits (nginx, load balancers, TLS, service
/// meshes) belong together as a platform category.
/// What: returns a single rule with proxy, load-balancer, TLS, and
/// service-mesh keywords.
/// Test: smoke-covered by the broad corpus.
pub(super) fn networking_rules() -> Vec<Rule> {
    vec![Rule {
        id: "kw-networking".into(),
        category: "networking".into(),
        subcategory: None,
        keywords: vec![
            " nginx".into(),
            "traefik".into(),
            "load balancer".into(),
            " cdn".into(),
            " ssl".into(),
            " tls".into(),
            "certificate".into(),
            "cert manager".into(),
            "letsencrypt".into(),
            "let's encrypt".into(),
            "reverse proxy".into(),
            "ingress controller".into(),
            "service mesh".into(),
            " istio".into(),
            "linkerd".into(),
            "envoy proxy".into(),
        ],
        patterns: vec![],
        priority: 65,
        confidence: 0.8,
    }]
}
