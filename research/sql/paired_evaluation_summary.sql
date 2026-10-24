-- DuckDB. Replace paths with the published inputs.
-- Required companion input: paired-expected-auctions.csv with one auction_id per expected evaluation auction.
-- evaluate.py emits exactly: auction_id, static_candidate, risk_candidate, status,
-- static_decay, risk_decay, static_future_feasible_score, risk_future_feasible_score,
-- static_gas_adjusted_value, risk_gas_adjusted_value, score_sacrifice, invalidation.
WITH all_varchar AS (
  SELECT * FROM read_csv_auto('paired-selections.csv', header = true, all_varchar = true)
), expected AS (
  SELECT trim(auction_id) AS auction_id
  FROM read_csv_auto('paired-expected-auctions.csv', header = true, all_varchar = true)
), validated AS (
  SELECT *,
    coalesce(trim(auction_id), '') <> '' AS valid_auction_id,
    coalesce(trim(static_candidate), '') <> '' AS valid_static_candidate,
    (risk_candidate IS NULL OR coalesce(trim(risk_candidate), '') <> '') AS valid_risk_candidate,
    status IN ('selected', 'no-eligible') AS valid_status,
    ((status = 'selected') = (coalesce(trim(risk_candidate), '') <> '')) AS valid_status_risk_consistency,
    try_cast(static_decay AS DOUBLE) IS NOT NULL AS valid_static_decay,
    (risk_decay IS NULL OR try_cast(risk_decay AS DOUBLE) IS NOT NULL) AS valid_risk_decay,
    try_cast(static_future_feasible_score AS DOUBLE) IS NOT NULL AS valid_static_score,
    (risk_future_feasible_score IS NULL OR try_cast(risk_future_feasible_score AS DOUBLE) IS NOT NULL) AS valid_risk_score,
    try_cast(static_gas_adjusted_value AS DOUBLE) IS NOT NULL AS valid_static_gas,
    (risk_gas_adjusted_value IS NULL OR try_cast(risk_gas_adjusted_value AS DOUBLE) IS NOT NULL) AS valid_risk_gas,
    (score_sacrifice IS NULL OR try_cast(score_sacrifice AS DOUBLE) IS NOT NULL) AS valid_sacrifice,
    (invalidation IS NULL OR invalidation IN ('true', 'false')) AS valid_invalidation
  FROM all_varchar
), contract AS (
  SELECT *, coalesce(valid_auction_id AND valid_static_candidate AND valid_risk_candidate AND valid_status
    AND valid_status_risk_consistency AND valid_static_decay AND valid_risk_decay AND valid_static_score AND valid_risk_score
    AND valid_static_gas AND valid_risk_gas AND valid_sacrifice AND valid_invalidation, false) AS valid_row
  FROM validated
), materialized AS (
  SELECT auction_id, count(*) AS materialized_count FROM contract GROUP BY auction_id
), expected_counts AS (
  SELECT auction_id, count(*) AS expected_count FROM expected GROUP BY auction_id
), counts AS (
  SELECT
    count(*) FILTER (WHERE e.auction_id IS NOT NULL AND m.auction_id IS NULL) AS missing_expected_auction_count,
    count(*) FILTER (WHERE e.auction_id IS NULL AND m.auction_id IS NOT NULL) AS extra_materialized_auction_count,
    coalesce((SELECT count(*) FROM expected) - count(*) FROM expected_counts, 0) AS duplicate_expected_auction_count,
    coalesce((SELECT count(*) FROM contract) - count(*) FROM materialized, 0) AS duplicate_materialized_auction_count,
    count(*) FILTER (WHERE m.materialized_count > 1) AS duplicate_materialized_id_count
  FROM expected_counts e FULL OUTER JOIN materialized m USING (auction_id)
), validation AS (
  SELECT count(*) FILTER (WHERE valid_row IS NOT TRUE) AS malformed_rows
  FROM contract
), flags AS (
  SELECT c.*, v.malformed_rows,
    (c.missing_expected_auction_count = 0 AND c.extra_materialized_auction_count = 0
     AND c.duplicate_expected_auction_count = 0 AND c.duplicate_materialized_auction_count = 0
     AND c.duplicate_materialized_id_count = 0 AND v.malformed_rows = 0) AS overall_valid
  FROM counts c CROSS JOIN validation v
), paired AS (
  SELECT * FROM contract WHERE valid_row IS TRUE AND coalesce(trim(risk_candidate), '') <> ''
), summary AS (
  SELECT 'static' AS policy, count(*) AS selected_count,
    avg(try_cast(static_decay AS DOUBLE)) AS mean_decay,
    avg(try_cast(static_future_feasible_score AS DOUBLE)) AS mean_future_feasible_score,
    avg(try_cast(static_gas_adjusted_value AS DOUBLE)) AS mean_gas_adjusted_value
  FROM paired
  UNION ALL SELECT 'risk', count(*), avg(try_cast(risk_decay AS DOUBLE)),
    avg(try_cast(risk_future_feasible_score AS DOUBLE)), avg(try_cast(risk_gas_adjusted_value AS DOUBLE)) FROM paired
), output AS (
  SELECT s.*, f.overall_valid, f.malformed_rows, f.missing_expected_auction_count,
    f.extra_materialized_auction_count, f.duplicate_expected_auction_count,
    f.duplicate_materialized_auction_count, f.duplicate_materialized_id_count
  FROM summary s CROSS JOIN flags f
)
SELECT * FROM output ORDER BY policy;
