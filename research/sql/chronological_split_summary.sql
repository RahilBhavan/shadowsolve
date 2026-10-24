-- DuckDB summary of the timestamp buckets used by split_observations.
-- Replace the path with the published CSV. Dataset rows carry decision_timestamp.
WITH d AS (
  SELECT DISTINCT auction_id, try_cast(decision_timestamp AS UBIGINT) AS decision_timestamp,
         try_cast(decision_block AS UBIGINT) AS decision_block
  FROM read_csv_auto('replay.csv', header = true, all_varchar = true)
), buckets AS (
  SELECT decision_timestamp, count(DISTINCT auction_id) AS auctions,
         min(decision_block) AS first_block, max(decision_block) AS last_block
  FROM d GROUP BY 1
)
SELECT row_number() OVER (ORDER BY decision_timestamp) AS bucket_number, *
FROM buckets ORDER BY decision_timestamp;
