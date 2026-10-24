"""Validated, content-addressed and atomic replay dataset publication."""
from dataclasses import asdict
from hashlib import sha256
import csv, io, json, os, tempfile
from pathlib import Path
from typing import Optional, Union
from .features import DecisionFeatures
from .types import ReplayLabel

def stable_json(v): return json.dumps(v,sort_keys=True,separators=(",",":"),ensure_ascii=True)
def _decimal(v): return str(v)
def _digest(v): return sha256(stable_json(v).encode()).hexdigest()
def label_record(l):
    if type(l) is not ReplayLabel: raise ValueError("labels must contain ReplayLabel values")
    return {"id":_digest([l.auction_id,l.candidate_id,l.horizon]),"auction_id":l.auction_id,"candidate_id":l.candidate_id,"network":l.network,"decision_block":_decimal(l.decision_block),"decision_hash":l.decision_hash,"horizon":_decimal(l.horizon),"future_block":_decimal(l.future_block),"future_block_hash":l.future_block_hash,"future_output":_decimal(l.future_output),"feasible":l.feasible,"margin_remaining":_decimal(l.margin_remaining),"margin_erosion":_decimal(l.margin_erosion),"score_decay":_decimal(l.score_decay)}
def feature_record(f):
    if type(f) is not DecisionFeatures: raise ValueError("features must be DecisionFeatures")
    r=asdict(f)
    for k in ("decision_block","receive_block","decision_timestamp","receive_timestamp","committed_output","auction_output","gas_proxy","initial_margin","route_count","split_count"): r[k]=_decimal(r[k])
    r["routes"]=[{**x,"sell_amount":_decimal(x["sell_amount"]),"reserve_in":_decimal(x["reserve_in"]),"reserve_out":_decimal(x["reserve_out"]),"fee_bps":_decimal(x["fee_bps"])} for x in r["routes"]]
    return r
def _atomic(path,data):
    path=Path(path); path.parent.mkdir(parents=True,exist_ok=True); fd,tmp=tempfile.mkstemp(prefix=f".{path.name}.",dir=path.parent)
    try:
        with os.fdopen(fd,"wb") as f: f.write(data); f.flush(); os.fsync(f.fileno())
        os.replace(tmp,path)
    finally:
        try: os.unlink(tmp)
        except FileNotFoundError: pass

def write_replay_dataset(path:Union[str,Path], features, labels, manifest_path:Optional[Union[str,Path]]=None, *, expected_horizons=(1,2,3), provenance=None):
    if type(features) not in (tuple,list) or not features or type(labels) not in (tuple,list) or not labels: raise ValueError("features and labels must be nonempty sequences")
    if type(expected_horizons) is not tuple or not expected_horizons or any(type(h) is not int or h <= 0 or h >= 2**64 for h in expected_horizons) or len(set(expected_horizons)) != len(expected_horizons): raise ValueError("expected_horizons must be unique positive u64 integers")
    expected_horizons=tuple(sorted(expected_horizons))
    p=provenance or {}; required=("source","state","candidate","code","config","schema")
    if set(p)!=set(required) or any(type(p[k]) is not str or len(p[k])!=64 or any(c not in "0123456789abcdef" for c in p[k]) for k in required): raise ValueError("provenance must contain lowercase SHA-256 hashes")
    if any(type(f) is not DecisionFeatures for f in features): raise ValueError("features must contain DecisionFeatures")
    versions={f.feature_schema_version for f in features}
    if len(versions)!=1: raise ValueError("mixed feature schema versions")
    fby={}; lby={}
    for f in features:
        k=(f.auction_id,f.candidate_id)
        if k in fby: raise ValueError("duplicate feature identity")
        fby[k]=f
    for l in labels:
        if type(l) is not ReplayLabel: raise ValueError("labels must contain ReplayLabel")
        k=(l.auction_id,l.candidate_id)
        if k not in fby: raise ValueError("orphan label")
        f=fby[k]
        if (l.network,l.decision_block,l.decision_hash)!=(f.network,f.decision_block,f.decision_hash): raise ValueError("label snapshot mismatch")
        if (k,l.horizon) in lby: raise ValueError("duplicate candidate+horizon")
        if l.margin_remaining != l.future_output-f.committed_output or l.margin_erosion != f.initial_margin-l.margin_remaining or l.score_decay != max(0,f.auction_output-l.future_output): raise ValueError("label equations mismatch")
        lby[(k,l.horizon)]=l
    if set(fby)!=set(k for k,h in lby): raise ValueError("missing feature or label")
    for k in fby:
        if {h for kk,h in lby if kk==k}!=set(expected_horizons): raise ValueError("incomplete or extra horizons")
    rows=[{**feature_record(fby[k]),**label_record(lby[k,h])} for k,h in sorted(lby)]
    jb="".join(stable_json(r)+"\n" for r in rows).encode(); fields=sorted({k for r in rows for k in r}); s=io.StringIO(newline=""); w=csv.DictWriter(s,fieldnames=fields,lineterminator="\n"); w.writeheader()
    for r in rows: w.writerow({k:stable_json(r[k]) if isinstance(r.get(k),(dict,list)) else r.get(k,"") for k in fields})
    cb=s.getvalue().encode(); digest=_digest(rows); root=Path(path); base=root.parent; jname=f"replay-{digest}.jsonl"; cname=f"replay-{digest}.csv"; mpath=Path(manifest_path) if manifest_path else root.with_suffix(".manifest.json")
    alternate_manifest=root.with_name(root.name+".manifest.json")
    targets=(root,root.with_suffix(".csv"),base/jname,base/cname,mpath,alternate_manifest)
    resolved=[target.resolve(strict=False) for target in targets]
    if len(set(resolved)) != len(resolved):
        raise ValueError("dataset output targets collide")
    _atomic(base/jname,jb); _atomic(base/cname,cb)
    _atomic(root,jb); _atomic(root.with_suffix('.csv'),cb)
    manifest={"dataset_version":"1","schema_version":"replay-label-v1","feature_schema_version":next(iter(versions)),"expected_horizons":list(expected_horizons),"records":len(rows),"input_sha256":digest,"identity_digest":_digest(sorted([[f.auction_id,f.candidate_id,f.network,f.decision_block,f.decision_hash] for f in features])),"identities":[{"auction_id":f.auction_id,"candidate_id":f.candidate_id,"network":f.network,"decision_block":f.decision_block,"decision_hash":f.decision_hash,"source_snapshot_hash":f.source_snapshot_hash,"state_snapshot_hash":f.state_snapshot_hash} for f in sorted(features,key=lambda x:(x.auction_id,x.candidate_id))],"jsonl":{"name":jname,"bytes":len(jb),"sha256":sha256(jb).hexdigest()},"csv":{"name":cname,"bytes":len(cb),"sha256":sha256(cb).hexdigest()},"provenance":p}
    mb=(stable_json(manifest)+"\n").encode(); _atomic(mpath,mb)
    if manifest_path is None and root.with_name(root.name+".manifest.json") != mpath: _atomic(root.with_name(root.name+".manifest.json"),mb)
    return manifest
