import hashlib, json
import pytest
from shadowsolve.features import decision_features
from shadowsolve.dataset import feature_record, write_replay_dataset
from shadowsolve.replay import replay
from shadowsolve.types import Candidate, PoolState, Route

def candidate(a="a",c="c",h="h10"):
    return Candidate(a,c,"mainnet",10,h,175,181,(Route("p",100),))
def state(block,h): return [PoolState("p","mainnet",block,h,1000,2000,30)]
PROV={k: "0"*64 for k in ("source","state","candidate","code","config","schema")}
def dataset_inputs():
    c=candidate(); f=decision_features(c,state(10,"h10"),100,10,100,source_snapshot_hash='0'*64,state_snapshot_hash='1'*64)
    return [f], [replay(c,state(10+h,f"h{h}"),h) for h in (1,2,3)]

def test_join_requires_exact_structural_snapshot_and_horizons(tmp_path):
    fs,ls=dataset_inputs(); out=tmp_path/"rows.jsonl"
    m=write_replay_dataset(out,fs,ls,provenance=PROV)
    assert (out.with_suffix(".csv")).exists()
    assert m["records"]==3 and m["jsonl"]["sha256"]==hashlib.sha256(out.read_bytes()).hexdigest()
    for bad in (ls[:2],ls+[replay(candidate(),state(14,"h4"),4)]):
        with pytest.raises(ValueError): write_replay_dataset(tmp_path/"bad.jsonl",fs,bad)

def test_rejects_manifest_collisions_before_writes(tmp_path):
    fs, ls = dataset_inputs(); out = tmp_path / "collision.jsonl"
    with pytest.raises(ValueError, match="targets collide"):
        write_replay_dataset(out, fs, ls, manifest_path=out.with_suffix(".csv"), provenance=PROV)
    assert not out.exists()


def test_rejects_duplicates_orphans_and_identity_mismatches(tmp_path):
    fs,ls=dataset_inputs(); out=tmp_path/"x.jsonl"
    with pytest.raises(ValueError): write_replay_dataset(out,fs,ls+[ls[0]])
    with pytest.raises(ValueError): write_replay_dataset(out,fs,[replay(candidate("other"),state(11,"h1"),1)]+ls[1:])
    wrong=list(ls); wrong[0]=replay(candidate(h="other"),state(11,"h1"),1)
    with pytest.raises(ValueError): write_replay_dataset(out,fs,wrong)

def test_outputs_are_byte_deterministic_and_flattened(tmp_path):
    fs,ls=dataset_inputs(); a=tmp_path/"a.jsonl"; b=tmp_path/"b.jsonl"
    ma=write_replay_dataset(a,fs,ls,provenance=PROV)
    mb=write_replay_dataset(b,fs,ls,provenance=PROV)
    assert a.read_bytes()==b.read_bytes(); assert a.with_suffix(".csv").read_bytes()==b.with_suffix(".csv").read_bytes(); assert ma==mb
    assert json.loads((tmp_path/"a.jsonl.manifest.json").read_text())["provenance"]["source"]==PROV["source"]

def test_feature_records_are_stable_and_routes_reject_duplicates():
    fs,_=dataset_inputs(); assert feature_record(fs[0])["route_count"]=="1"
    with pytest.raises(ValueError): Candidate("a","c","mainnet",1,"h",1,1,(Route("p",1),Route("p",1)))
