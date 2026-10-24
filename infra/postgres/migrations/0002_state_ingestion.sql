CREATE TABLE IF NOT EXISTS blocks (
    network text NOT NULL CHECK (length(btrim(network)) > 0),
    number bigint NOT NULL CHECK (number >= 0),
    hash text NOT NULL CHECK (length(btrim(hash)) > 0),
    parent_hash text NOT NULL CHECK (length(btrim(parent_hash)) > 0),
    timestamp bigint NOT NULL CHECK (timestamp >= 0),
    canonical boolean NOT NULL DEFAULT false,
    PRIMARY KEY (network, hash),
    UNIQUE (network, number, hash)
);
CREATE UNIQUE INDEX IF NOT EXISTS blocks_one_canonical_per_height ON blocks(network, number) WHERE canonical;
CREATE TABLE IF NOT EXISTS pool_snapshots (
    network text NOT NULL CHECK (length(btrim(network)) > 0),
    pool text NOT NULL CHECK (length(btrim(pool)) > 0),
    block_hash text NOT NULL CHECK (length(btrim(block_hash)) > 0),
    timestamp bigint NOT NULL CHECK (timestamp >= 0),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object' AND payload <> '{}'::jsonb),
    PRIMARY KEY(network, pool, block_hash),
    FOREIGN KEY(network, block_hash) REFERENCES blocks(network, hash)
);
CREATE TABLE IF NOT EXISTS ingestion_cursors (
    network text NOT NULL CHECK (length(btrim(network)) > 0),
    source text NOT NULL CHECK (length(btrim(source)) > 0),
    next_block bigint NOT NULL CHECK (next_block >= 0),
    block_hash text NOT NULL CHECK (length(btrim(block_hash)) > 0),
    PRIMARY KEY(network, source),
    FOREIGN KEY(network, block_hash) REFERENCES blocks(network, hash)
);
