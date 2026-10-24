CREATE TABLE IF NOT EXISTS auctions (id text PRIMARY KEY, payload jsonb NOT NULL);
CREATE TABLE IF NOT EXISTS candidates (
    id bigserial PRIMARY KEY,
    auction_id text NOT NULL REFERENCES auctions(id),
    payload jsonb NOT NULL,
    UNIQUE (auction_id, id),
    UNIQUE (auction_id, payload)
);
CREATE TABLE IF NOT EXISTS predictions (
    id text PRIMARY KEY,
    auction_id text NOT NULL REFERENCES auctions(id),
    candidate_id bigint,
    payload jsonb NOT NULL,
    FOREIGN KEY (auction_id, candidate_id) REFERENCES candidates(auction_id, id)
);
CREATE TABLE IF NOT EXISTS notifications (id text PRIMARY KEY, payload jsonb NOT NULL);
CREATE TABLE IF NOT EXISTS replay_labels (
    id text PRIMARY KEY,
    auction_id text NOT NULL REFERENCES auctions(id),
    candidate_id bigint,
    label text NOT NULL,
    FOREIGN KEY (auction_id, candidate_id) REFERENCES candidates(auction_id, id)
);
