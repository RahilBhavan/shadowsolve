-- DuckDB. Replace the path with the manifest emitted by write_replay_dataset.
WITH m AS (
  SELECT * FROM read_json_auto('replay.manifest.json', format = 'unstructured')
), checks AS (
  SELECT 'manifest_dataset_version' AS check_name,
         count(*) FILTER (WHERE dataset_version <> '1') AS violations FROM m
  UNION ALL SELECT 'manifest_schema_version', count(*) FILTER (WHERE schema_version <> 'replay-label-v1') FROM m
  UNION ALL SELECT 'manifest_jsonl_sha256', count(*) FILTER (WHERE NOT regexp_matches(jsonl.sha256, '^[0-9a-f]{64}$')) FROM m
  UNION ALL SELECT 'manifest_csv_sha256', count(*) FILTER (WHERE NOT regexp_matches(csv.sha256, '^[0-9a-f]{64}$')) FROM m
  UNION ALL SELECT 'manifest_input_sha256', count(*) FILTER (WHERE NOT regexp_matches(input_sha256, '^[0-9a-f]{64}$')) FROM m
  UNION ALL SELECT 'manifest_identity_sha256', count(*) FILTER (WHERE NOT regexp_matches(identity_digest, '^[0-9a-f]{64}$')) FROM m
  UNION ALL SELECT 'manifest_provenance_hashes', count(*) FILTER (WHERE NOT regexp_matches(provenance.source, '^[0-9a-f]{64}$') OR NOT regexp_matches(provenance.state, '^[0-9a-f]{64}$') OR NOT regexp_matches(provenance.candidate, '^[0-9a-f]{64}$') OR NOT regexp_matches(provenance.code, '^[0-9a-f]{64}$') OR NOT regexp_matches(provenance.config, '^[0-9a-f]{64}$') OR NOT regexp_matches(provenance.schema, '^[0-9a-f]{64}$')) FROM m
)
SELECT * FROM checks ORDER BY check_name;
