#!/usr/bin/env bash
# Notion MCP server for the supervisor, using the webapp-companion Notion integration
# (the owner, 2026-09-28: "pull the webapp-companion notion connector ... and add to supervisor").
# The key is read at launch from AWS Secrets Manager webapp-companion/production
# (field NOTION_API_KEY, profile cto) and never written to disk.
set -euo pipefail
NOTION_TOKEN=$(aws secretsmanager get-secret-value --secret-id webapp-companion/production \
  --profile "${NOTION_AWS_PROFILE:-cto}" --region us-east-1 --query SecretString --output text \
  | python3 -c 'import json,sys;print(json.load(sys.stdin)["NOTION_API_KEY"],end="")')
export NOTION_TOKEN
exec npx -y @notionhq/notion-mcp-server@2.5.2
