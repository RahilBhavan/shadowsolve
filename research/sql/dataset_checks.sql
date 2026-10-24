-- DuckDB. Replace the path with the published CSV.
-- Canonical decimal strings are checked lexically; casts are only secondary checks.
CREATE OR REPLACE TEMP MACRO valid_u128(s) AS coalesce(
  regexp_matches(s, '^(0|[1-9][0-9]*)$')
  AND (length(s) < 39 OR (length(s) = 39 AND s <= '340282366920938463463374607431768211455')), false);
CREATE OR REPLACE TEMP MACRO valid_u64(s) AS coalesce(
  regexp_matches(s, '^(0|[1-9][0-9]*)$')
  AND (length(s) < 20 OR (length(s) = 20 AND s <= '18446744073709551615')), false);
-- ReplayLabel signed bounds are symmetric [-U128_MAX, U128_MAX], not i128 bounds.
CREATE OR REPLACE TEMP MACRO valid_signed_u128(s) AS coalesce(
  regexp_matches(s, '^(0|[1-9][0-9]*|-[1-9][0-9]*)$')
  AND (length(CASE WHEN left(s, 1) = '-' THEN substr(s, 2) ELSE s END) < 39
       OR (length(CASE WHEN left(s, 1) = '-' THEN substr(s, 2) ELSE s END) = 39
           AND CASE WHEN left(s, 1) = '-' THEN substr(s, 2) ELSE s END <= '340282366920938463463374607431768211455')),
  false);
WITH d AS (
  SELECT * FROM read_csv_auto('replay.csv', header = true, all_varchar = true)
), parsed AS (
  SELECT d.*,
    try_cast(decision_block AS BIGNUM) AS decision_block_n,
    try_cast(horizon AS BIGNUM) AS horizon_n,
    try_cast(future_block AS BIGNUM) AS future_block_n,
    try_cast(committed_output AS BIGNUM) AS committed_output_n,
    try_cast(initial_margin AS BIGNUM) AS initial_margin_n,
    try_cast(auction_output AS BIGNUM) AS auction_output_n,
    try_cast(future_output AS BIGNUM) AS future_output_n,
    try_cast(margin_remaining AS BIGNUM) AS margin_remaining_n,
    try_cast(margin_erosion AS BIGNUM) AS margin_erosion_n,
    try_cast(score_decay AS BIGNUM) AS score_decay_n
  FROM d
), duplicates AS (
  SELECT auction_id, candidate_id, horizon FROM parsed
  GROUP BY auction_id, candidate_id, horizon HAVING count(*) > 1
), checks AS (
  SELECT 'missing_required_identity' AS check_name, count(*) FILTER (WHERE coalesce(trim(auction_id), '') = '' OR coalesce(trim(candidate_id), '') = '' OR coalesce(trim(id), '') = '') AS violations FROM parsed
  UNION ALL SELECT 'duplicate_identity', count(*) FROM duplicates
  -- SQL cannot recompute Python canonical JSON identity; manifest/Python verification is required.
  UNION ALL SELECT 'canonical_id_digest_verification_delegated', NULL::BIGINT
  UNION ALL SELECT 'invalid_identity_fields_or_hash_shape', count(*) FILTER (WHERE coalesce(regexp_matches(id, '^[0-9a-f]{64}$'), false) = false) FROM parsed
  UNION ALL SELECT 'missing_feasible', count(*) FILTER (WHERE feasible IS NULL OR feasible NOT IN ('true', 'false')) FROM parsed
  UNION ALL SELECT 'invalid_unsigned_decimal', count(*) FILTER (WHERE NOT (valid_u64(decision_block) AND valid_u64(horizon) AND valid_u64(future_block) AND valid_u128(committed_output) AND valid_u128(auction_output) AND valid_u128(future_output) AND valid_u128(score_decay))) FROM parsed
  UNION ALL SELECT 'invalid_signed_replay_margin', count(*) FILTER (WHERE NOT (valid_signed_u128(initial_margin) AND valid_signed_u128(margin_remaining) AND valid_signed_u128(margin_erosion))) FROM parsed
  UNION ALL SELECT 'integer_cast_failure', count(*) FILTER (WHERE decision_block_n IS NULL OR horizon_n IS NULL OR future_block_n IS NULL OR committed_output_n IS NULL OR initial_margin_n IS NULL OR auction_output_n IS NULL OR future_output_n IS NULL OR margin_remaining_n IS NULL OR margin_erosion_n IS NULL OR score_decay_n IS NULL) FROM parsed
  UNION ALL SELECT 'label_margin_equation', count(*) FILTER (WHERE margin_remaining_n IS NULL OR future_output_n IS NULL OR committed_output_n IS NULL OR margin_remaining_n <> future_output_n - committed_output_n) FROM parsed
  UNION ALL SELECT 'label_erosion_equation', count(*) FILTER (WHERE margin_erosion_n IS NULL OR initial_margin_n IS NULL OR margin_remaining_n IS NULL OR margin_erosion_n <> initial_margin_n - margin_remaining_n) FROM parsed
  UNION ALL SELECT 'label_decay_equation', count(*) FILTER (WHERE score_decay_n IS NULL OR auction_output_n IS NULL OR future_output_n IS NULL OR score_decay_n <> greatest(0::BIGNUM, auction_output_n - future_output_n)) FROM parsed
)
SELECT * FROM checks ORDER BY check_name;
