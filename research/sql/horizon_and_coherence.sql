-- DuckDB. Replace the path with the published CSV.
CREATE OR REPLACE TEMP MACRO valid_u128(s) AS coalesce(
  regexp_matches(s, '^(0|[1-9][0-9]*)$')
  AND (length(s) < 39 OR (length(s) = 39 AND s <= '340282366920938463463374607431768211455')), false);
CREATE OR REPLACE TEMP MACRO valid_u64(s) AS coalesce(
  regexp_matches(s, '^(0|[1-9][0-9]*)$')
  AND (length(s) < 20 OR (length(s) = 20 AND s <= '18446744073709551615')), false);
WITH d AS (
  SELECT * FROM read_csv_auto('replay.csv', header = true, all_varchar = true)
), p AS (
  SELECT *, try_cast(decision_block AS BIGNUM) AS decision_block_n,
           try_cast(horizon AS BIGNUM) AS horizon_n,
           try_cast(future_block AS BIGNUM) AS future_block_n,
           valid_u64(decision_block) AS valid_decision_block,
           valid_u64(horizon) AS valid_horizon,
           valid_u64(future_block) AS valid_future_block
  FROM d
), horizon_rows AS (
  SELECT auction_id, candidate_id, horizon, count(*) AS rows_at_horizon,
    count(*) FILTER (WHERE coalesce(trim(future_block), '') = '' OR coalesce(trim(future_block_hash), '') = '') AS missing_future_fields
  FROM p WHERE horizon IN ('1', '2', '3') GROUP BY auction_id, candidate_id, horizon
), per_candidate AS (
  SELECT p.auction_id, p.candidate_id,
    count(*) AS total_rows,
    count(*) FILTER (WHERE p.horizon IN ('1', '2', '3')) AS expected_rows,
    count(DISTINCT p.horizon) FILTER (WHERE p.horizon IN ('1', '2', '3')) AS horizon_count,
    count(*) FILTER (WHERE p.horizon IS NULL OR p.horizon NOT IN ('1', '2', '3')) AS invalid_horizon_rows,
    count(DISTINCT p.network) FILTER (WHERE coalesce(trim(p.network), '') <> '') AS network_count,
    count(DISTINCT p.decision_block) FILTER (WHERE coalesce(trim(p.decision_block), '') <> '') AS decision_block_count,
    count(DISTINCT p.decision_hash) FILTER (WHERE coalesce(trim(p.decision_hash), '') <> '') AS decision_hash_count,
    count(*) FILTER (WHERE coalesce(trim(p.network), '') = '' OR coalesce(trim(p.decision_hash), '') = '' OR NOT (p.valid_decision_block AND p.valid_horizon AND p.valid_future_block)) AS bad_parse_rows,
    count(*) FILTER (WHERE p.horizon IN ('1', '2', '3') AND (p.future_block_n IS NULL OR p.decision_block_n IS NULL OR p.horizon_n IS NULL OR p.future_block_n <> p.decision_block_n + p.horizon_n)) AS bad_future_block_rows,
    count(*) FILTER (WHERE h.rows_at_horizon <> 1 OR h.missing_future_fields <> 0) AS bad_horizon_rows,
    count(DISTINCT p.future_block_hash) FILTER (WHERE p.horizon IN ('1', '2', '3') AND coalesce(trim(p.future_block_hash), '') <> '') AS future_hash_count
  FROM p LEFT JOIN horizon_rows h USING (auction_id, candidate_id, horizon)
  GROUP BY p.auction_id, p.candidate_id
)
SELECT *,
  (total_rows = 3 AND expected_rows = 3 AND horizon_count = 3 AND invalid_horizon_rows = 0
   AND network_count = 1 AND decision_block_count = 1 AND decision_hash_count = 1 AND bad_parse_rows = 0
   AND bad_horizon_rows = 0 AND bad_future_block_rows = 0 AND future_hash_count = 3) IS TRUE AS coherent
FROM per_candidate ORDER BY auction_id, candidate_id;
