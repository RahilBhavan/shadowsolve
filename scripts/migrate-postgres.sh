#!/usr/bin/env bash
# Apply ShadowSolve Postgres migrations to an existing Compose volume.
set -euo pipefail
compose=(docker compose)
user="${POSTGRES_USER:-shadowsolve_dev}"
db="${POSTGRES_DB:-shadowsolve}"
for file in \
  /docker-entrypoint-initdb.d/0001_init.sql \
  /docker-entrypoint-initdb.d/0002_state_ingestion.sql \
  /docker-entrypoint-initdb.d/0003_indexes.sql \
  /docker-entrypoint-initdb.d/0004_advisory_provenance.sql
do
  echo "Applying $file"
  "${compose[@]}" exec -T postgres psql -U "$user" -d "$db" -v ON_ERROR_STOP=1 -f "$file"
done
echo "Migrations applied (idempotent statements may no-op on already-applied schemas)."
