-- DORA metrics infrastructure (issues #207, #208, #212, #213).
--
-- Adds the canonical deployment + incident + failure tables that all four
-- DORA metrics (Deployment Frequency, Lead Time for Changes, Change Failure
-- Rate, Mean Time To Recovery) join through, plus pre-computed SQL views
-- for the read-side hot paths.
--
-- Naming convention: `fact_*` tables are the canonical "single source of
-- truth" — each row is an immutable observation tagged with a stable
-- primary key. Other tables (`deployment_failures`) hold derived joins
-- between facts.

-- =============================================================================
-- fact_deployments (#212)
-- =============================================================================
-- Canonical deploy-event hub. Every DORA query joins through this table.
--
-- `deploy_id` is the primary key from the upstream source (e.g. GitHub
-- Actions run id, Release tag name) so re-ingest is idempotent.
CREATE TABLE IF NOT EXISTS fact_deployments (
    deploy_id        TEXT PRIMARY KEY,
    repo             TEXT NOT NULL,
    environment      TEXT NOT NULL DEFAULT 'production',
    triggered_at     TIMESTAMP,
    completed_at     TIMESTAMP,
    status           TEXT,
    git_sha          TEXT,
    git_tag          TEXT,
    triggered_by_pr  INTEGER,
    source           TEXT
);

CREATE INDEX IF NOT EXISTS idx_fact_deployments_repo
    ON fact_deployments(repo);
CREATE INDEX IF NOT EXISTS idx_fact_deployments_triggered_at
    ON fact_deployments(triggered_at);
CREATE INDEX IF NOT EXISTS idx_fact_deployments_env_status
    ON fact_deployments(environment, status);
CREATE INDEX IF NOT EXISTS idx_fact_deployments_git_sha
    ON fact_deployments(git_sha);

-- =============================================================================
-- fact_incidents (#213)
-- =============================================================================
-- Production-incident observations from external sources (Datadog,
-- PagerDuty, JIRA SRE). `mttr_hours` is denormalised on write so DORA
-- aggregation queries are cheap.
CREATE TABLE IF NOT EXISTS fact_incidents (
    incident_id        TEXT PRIMARY KEY,
    source             TEXT,
    detected_at        TIMESTAMP,
    resolved_at        TIMESTAMP,
    mttr_hours         REAL,
    severity           TEXT,
    triggering_deploy  TEXT,
    repo               TEXT,
    jira_ticket        TEXT,
    FOREIGN KEY(triggering_deploy) REFERENCES fact_deployments(deploy_id)
        ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_fact_incidents_repo
    ON fact_incidents(repo);
CREATE INDEX IF NOT EXISTS idx_fact_incidents_detected_at
    ON fact_incidents(detected_at);
CREATE INDEX IF NOT EXISTS idx_fact_incidents_source
    ON fact_incidents(source);

-- =============================================================================
-- deployment_failures (#208)
-- =============================================================================
-- Derived join between a fact_deployments row and the commit that
-- triggered a failure (and the recovery commit, when known). Populated
-- by the `tga dora` analysis pass using the `failure_signals` config.
CREATE TABLE IF NOT EXISTS deployment_failures (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    deploy_id             TEXT,
    failure_commit_sha    TEXT,
    recovery_commit_sha   TEXT,
    detected_at           TIMESTAMP,
    recovered_at          TIMESTAMP,
    FOREIGN KEY(deploy_id) REFERENCES fact_deployments(deploy_id)
        ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_deployment_failures_deploy_id
    ON deployment_failures(deploy_id);
CREATE INDEX IF NOT EXISTS idx_deployment_failures_failure_commit
    ON deployment_failures(failure_commit_sha);

-- =============================================================================
-- DORA SQL Views (#212)
-- =============================================================================
-- Each view is a thin convenience wrapper around `fact_deployments` /
-- `fact_incidents` so reports can `SELECT * FROM v_deployment_frequency`
-- without re-writing the same aggregation in three different commands.

-- v_deployment_frequency: weekly bucket of successful production deploys.
-- ISO week format matches the existing `tga` weekly aggregator.
CREATE VIEW IF NOT EXISTS v_deployment_frequency AS
SELECT strftime('%Y-W%W', triggered_at) AS week_label,
       repo,
       COUNT(*)                          AS deploy_count
FROM   fact_deployments
WHERE  environment = 'production'
  AND  status = 'success'
GROUP BY week_label, repo;

-- v_lead_time: hours between commit author date and successful production
-- deploy of that SHA. Joins on `git_sha` which is indexed.
CREATE VIEW IF NOT EXISTS v_lead_time AS
SELECT d.deploy_id,
       d.repo,
       d.triggered_at                                                       AS deployed_at,
       c.timestamp                                                          AS authored_at,
       (julianday(d.triggered_at) - julianday(c.timestamp)) * 24.0          AS lead_time_hours
FROM   fact_deployments d
JOIN   commits c ON c.sha = d.git_sha
WHERE  d.environment = 'production'
  AND  d.status = 'success';

-- v_mttr: hours between incident detection and resolution.
-- Sourced from `fact_incidents.mttr_hours` (already denormalised) so this
-- view stays cheap even with millions of incidents.
CREATE VIEW IF NOT EXISTS v_mttr AS
SELECT incident_id,
       repo,
       source,
       detected_at,
       resolved_at,
       mttr_hours
FROM   fact_incidents
WHERE  mttr_hours IS NOT NULL;

-- v_change_failure_rate: per-repo weekly CFR computed as
-- (failed_deploys / total_deploys) using `deployment_failures` as the
-- failure set.
CREATE VIEW IF NOT EXISTS v_change_failure_rate AS
SELECT strftime('%Y-W%W', d.triggered_at)                            AS week_label,
       d.repo                                                        AS repo,
       COUNT(DISTINCT d.deploy_id)                                   AS total_deploys,
       COUNT(DISTINCT df.deploy_id)                                  AS failed_deploys,
       CASE WHEN COUNT(DISTINCT d.deploy_id) = 0 THEN 0.0
            ELSE CAST(COUNT(DISTINCT df.deploy_id) AS REAL)
                 / CAST(COUNT(DISTINCT d.deploy_id) AS REAL)
       END                                                           AS cfr
FROM   fact_deployments d
LEFT JOIN deployment_failures df ON df.deploy_id = d.deploy_id
WHERE  d.environment = 'production'
GROUP BY week_label, d.repo;
