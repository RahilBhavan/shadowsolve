CREATE INDEX IF NOT EXISTS candidates_auction_id_idx ON candidates(auction_id);
CREATE INDEX IF NOT EXISTS predictions_auction_id_idx ON predictions(auction_id);
CREATE INDEX IF NOT EXISTS notifications_payload_idx ON notifications USING gin(payload);
CREATE INDEX IF NOT EXISTS pool_snapshots_network_timestamp_idx ON pool_snapshots(network, timestamp DESC);
CREATE INDEX IF NOT EXISTS ingestion_cursors_network_next_block_idx ON ingestion_cursors(network, next_block);
