//! The bus's per-source and per-project counters behind
//! `events.console_metrics` (#10906, owner ruling D95).
//!
//! Why: `events.health` reports totals only. D95 asks which harness and which
//! project the traffic comes from, and how many inter-session messages were
//! sent and received, pulled over the bus socket.
//! What: [`Breakdown`] counts every accepted event by `(source, domain)`, and
//! every `message`-domain event by direction, once in total and once under its
//! project. An event with no project, or a blank one, counts under
//! [`UNATTRIBUTED_PROJECT`]. At most [`MAX_PROJECTS`] distinct projects get
//! their own entry; a later new project counts under [`OVERFLOW_PROJECT`], so
//! a producer cannot grow the map without limit. Every key but the project is
//! a `&'static str` from a closed set. [`Breakdown::to_json`] renders the
//! `metrics` shape `trusty_common::control_bus` documents for
//! `METHOD_EVENTS_CONSOLE_METRICS`, which follows the `by_*` convention in
//! `trusty_common::console_metrics::by_convention`.
//! Test: `super::breakdown_tests`.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use trusty_common::control_bus::{
    HarnessEvent, HarnessPayload, MessageDirection, OVERFLOW_PROJECT, UNATTRIBUTED_PROJECT,
};

/// Most distinct projects with their own `by_project` entry.
///
/// Why: `project` is producer-supplied, and the bus runs for the host's whole
/// session. 256 is well above the projects one host works on at once.
pub(crate) const MAX_PROJECTS: usize = 256;

/// One source's events, split by payload domain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SourceCounts {
    events: u64,
    by_domain: BTreeMap<&'static str, u64>,
}

/// The counters for one scope: the whole bus, or one project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Counts {
    events: u64,
    by_source: BTreeMap<&'static str, SourceCounts>,
    messages: BTreeMap<&'static str, u64>,
}

impl Counts {
    fn add(&mut self, source: &'static str, domain: &'static str, direction: Option<&'static str>) {
        self.events += 1;
        let per_source = self.by_source.entry(source).or_default();
        per_source.events += 1;
        *per_source.by_domain.entry(domain).or_default() += 1;
        if let Some(direction) = direction {
            *self.messages.entry(direction).or_default() += 1;
        }
    }

    fn to_map(&self) -> Map<String, Value> {
        let by_source: Map<String, Value> = self
            .by_source
            .iter()
            .map(|(source, counts)| {
                let body = json!({"events": counts.events, "by_domain": counts.by_domain});
                ((*source).to_string(), body)
            })
            .collect();
        // Both directions are always present, so a reader never tells "zero"
        // from "not reported".
        let mut messages = Map::new();
        for direction in [MessageDirection::Sent, MessageDirection::Received] {
            let count = self.messages.get(direction.as_str()).copied();
            messages.insert(direction.as_str().to_string(), json!(count.unwrap_or(0)));
        }
        let mut map = Map::new();
        map.insert("events".to_string(), json!(self.events));
        map.insert("by_source".to_string(), Value::Object(by_source));
        map.insert("messages".to_string(), Value::Object(messages));
        map
    }
}

/// The bus's breakdown counters since start.
///
/// Why/What: see the module docs. Plain data: the bus mutates it under its
/// ring lock, inside `EventBus::ingest`, and hands out clones.
/// Test: `super::breakdown_tests`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Breakdown {
    total: Counts,
    by_project: BTreeMap<String, Counts>,
    max_projects: usize,
}

impl Default for Breakdown {
    fn default() -> Self {
        Self::with_max_projects(MAX_PROJECTS)
    }
}

impl Breakdown {
    /// An empty breakdown that gives at most `max_projects` projects their own
    /// entry.
    pub(crate) fn with_max_projects(max_projects: usize) -> Self {
        Self {
            total: Counts::default(),
            by_project: BTreeMap::new(),
            max_projects,
        }
    }

    /// Count one accepted event.
    ///
    /// Test: `super::breakdown_tests::events_count_by_source_domain_and_project`,
    /// `super::breakdown_tests::a_missing_or_blank_project_counts_as_unattributed`,
    /// `super::breakdown_tests::projects_past_the_cap_count_as_overflow`.
    pub(crate) fn record(&mut self, event: &HarnessEvent) {
        let source = event.source.as_str();
        let domain = event.payload.domain();
        let direction = match &event.payload {
            HarnessPayload::Message(message) => Some(message.direction.as_str()),
            _ => None,
        };
        self.total.add(source, domain, direction);
        let project = self.project_key(event.project.as_deref());
        self.by_project
            .entry(project)
            .or_default()
            .add(source, domain, direction);
    }

    /// The `by_project` key for `project`.
    fn project_key(&self, project: Option<&str>) -> String {
        // #10906: blank reads as absent; a producer that knows no project
        // should omit it, and an empty key would be unreadable in a breakdown.
        let Some(project) = project.filter(|p| !p.trim().is_empty()) else {
            return UNATTRIBUTED_PROJECT.to_string();
        };
        if self.by_project.contains_key(project) || self.by_project.len() < self.max_projects {
            project.to_string()
        } else {
            OVERFLOW_PROJECT.to_string()
        }
    }

    /// The report's `metrics` payload.
    ///
    /// What: `{uptime_secs, events, by_source, messages, by_project}`; each
    /// `by_project` value has the same `events`, `by_source` and `messages`.
    /// Test: `super::breakdown_tests::events_count_by_source_domain_and_project`,
    /// `super::breakdown_tests::an_empty_bus_reports_every_key`.
    pub(crate) fn to_json(&self, uptime_secs: u64) -> Value {
        let mut metrics = self.total.to_map();
        let by_project: Map<String, Value> = self
            .by_project
            .iter()
            .map(|(project, counts)| (project.clone(), Value::Object(counts.to_map())))
            .collect();
        metrics.insert("uptime_secs".to_string(), json!(uptime_secs));
        metrics.insert("by_project".to_string(), Value::Object(by_project));
        Value::Object(metrics)
    }
}
