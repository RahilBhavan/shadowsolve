import json
import pytest

from shadowsolve.ingest import ingest, read_jsonl


def test_jsonl_rejects_blank_malformed_non_object_and_duplicate_nested_keys(tmp_path):
    cases = ["\n", "not json\n", "[]\n", '{"a": 1, "a": 2}\n', '{"a": {"b": 1, "b": 2}}\n']
    for index, text in enumerate(cases):
        path = tmp_path / f"bad-{index}.jsonl"
        path.write_text(text)
        with pytest.raises(ValueError, match="line 1"):
            read_jsonl(path)


def test_deterministic_ingestion(tmp_path):
    candidate = {"auction_id":"a","candidate_id":"c","network":"n","decision_block":"10","decision_hash":"e","committed_output":"1","auction_output":"2","routes":[{"pool_id":"p","sell_amount":"1"}]}
    state = {"pool_id":"p","network":"n","block":"10","block_hash":"e","reserve_in":"100","reserve_out":"100","fee_bps":"30","concentrated":False}
    future = dict(state, block="11", block_hash="f")
    def write(name, rows):
        path=tmp_path/name
        path.write_text("".join(json.dumps(row, sort_keys=True)+"\n" for row in rows))
        return path
    args=(write("c.jsonl",[candidate]), write("d.jsonl",[state]), write("f.jsonl",[{"horizon":"1","state":future}]))
    provenance={k: "a"*64 for k in ("source","state","candidate","code","config","schema")}
    ingest(*args, tmp_path/"out.jsonl", tmp_path/"manifest.json", source_snapshot_hash="a"*64, state_snapshot_hash="b"*64, provenance=provenance, expected_horizons=(1,))
    first=(tmp_path/"out.jsonl").read_bytes()
    ingest(*args, tmp_path/"out2.jsonl", tmp_path/"manifest2.json", source_snapshot_hash="a"*64, state_snapshot_hash="b"*64, provenance=provenance, expected_horizons=(1,))
    assert first == (tmp_path/"out2.jsonl").read_bytes()
