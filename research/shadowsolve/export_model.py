"""Canonical audited model artifacts."""
from hashlib import sha256
from datetime import datetime
import json, math, os, re, tempfile
from pathlib import Path
from .train import BaseRateModel, BoostedTreeModel, LogisticModel
from .calibrate import Calibrated

_KEYS={"artifact_version","model","calibration","model_version","feature_schema_hash","training_data_hash","calibration_data_hash","code_hash","config_hash","created_at","artifact_sha256"}
_HASHES=("feature_schema_hash","training_data_hash","calibration_data_hash","code_hash","config_hash")
_NAME=re.compile(r"^[A-Za-z][A-Za-z0-9_]{0,63}$")
_CLIP=1e-15

def _json(v): return json.dumps(v,sort_keys=True,separators=(",",":"),ensure_ascii=True,allow_nan=False)
def _num(v):
    if type(v) not in (int,float): raise ValueError("invalid numeric")
    try: v=float(v)
    except OverflowError: raise ValueError("invalid numeric")
    if not math.isfinite(v): raise ValueError("invalid numeric")
    return v
def _hash(v):
    if type(v) is not str or not re.fullmatch(r"[0-9a-f]{64}",v): raise ValueError("invalid hash")
def _validate(a,verify=True):
    if type(a) is not dict or set(a)!=_KEYS: raise ValueError("artifact fields mismatch")
    if a["artifact_version"]!="1" or a["model_version"]!="1": raise ValueError("unsupported version")
    for k in _HASHES+ ("artifact_sha256",): _hash(a[k])
    try: dt=datetime.strptime(a["created_at"],"%Y-%m-%dT%H:%M:%SZ")
    except (TypeError,ValueError): raise ValueError("invalid created_at")
    if dt.strftime("%Y-%m-%dT%H:%M:%SZ")!=a["created_at"]: raise ValueError("invalid created_at")
    c=a["calibration"]
    if type(c) is not dict or set(c)!={"method","slope","intercept"} or c["method"] not in ("none","platt"): raise ValueError("invalid calibration")
    slope,intercept=_num(c["slope"]),_num(c["intercept"])
    if c["method"]=="none" and (slope!=1 or intercept!=0): raise ValueError("identity calibration must be neutral")
    m=a["model"]
    if type(m) is not dict or m.get("kind") not in ("base-rate","logistic","boosted-tree"): raise ValueError("invalid model")
    names=m.get("feature_names")
    if type(names) is not list or not names or len(set(names))!=len(names) or any(type(n) is not str or not _NAME.fullmatch(n) for n in names): raise ValueError("invalid feature names")
    w=len(names)
    if m["kind"]=="base-rate":
        if set(m)!={"kind","feature_names","rate"} or not 0<=_num(m["rate"])<=1: raise ValueError("invalid base rate")
    elif m["kind"] == "boosted-tree":
        if set(m)!={"kind","feature_names","intercept","learning_rate","trees"} or type(m["trees"]) is not list or not 0 <= len(m["trees"]) <= 64: raise ValueError("invalid boosted tree")
        _num(m["intercept"]); rate = _num(m["learning_rate"])
        if not 0 < rate <= 1: raise ValueError("invalid boosted tree")
        for tree in m["trees"]:
            if type(tree) is not dict or set(tree)!={"feature_index","threshold","left_value","right_value"} or type(tree["feature_index"]) is not int or not 0 <= tree["feature_index"] < w: raise ValueError("invalid boosted tree")
            for key in ("threshold","left_value","right_value"): _num(tree[key])
    else:
        if set(m)!={"kind","feature_names","means","scales","weights","intercept"}: raise ValueError("invalid logistic")
        for k in ("means","scales","weights"):
            if type(m[k]) is not list or len(m[k])!=w or any(not math.isfinite(_num(x)) for x in m[k]): raise ValueError("invalid width")
        if any(x<=0 for x in m["scales"]): raise ValueError("invalid scales")
        _num(m["intercept"])
    if verify:
        b={k:v for k,v in a.items() if k!="artifact_sha256"}
        if sha256(_json(b).encode()).hexdigest()!=a["artifact_sha256"]: raise ValueError("artifact hash mismatch")
    return a

def export_model(model,path,*,calibration=None,feature_schema_hash,training_data_hash,calibration_data_hash,code_hash,config_hash,created_at,artifact_version="1",model_version="1"):
    raw=model.model if isinstance(model,Calibrated) else model
    if isinstance(raw,BaseRateModel): m={"kind":"base-rate","feature_names":list(raw.feature_names),"rate":raw.rate}
    elif isinstance(raw,LogisticModel): m={"kind":"logistic","feature_names":list(raw.feature_names),"means":list(raw.standardizer.means),"scales":list(raw.standardizer.scales),"weights":list(raw.weights),"intercept":raw.intercept}
    elif isinstance(raw,BoostedTreeModel): m={"kind":"boosted-tree","feature_names":list(raw.feature_names),"intercept":raw.intercept,"learning_rate":raw.learning_rate,"trees":[{"feature_index":i,"threshold":t,"left_value":l,"right_value":r} for i,t,l,r in raw.trees]}
    else: raise ValueError("unsupported model")
    if isinstance(model,Calibrated): calibration={"method":"none" if model.method=="identity" else model.method,"slope":model.slope,"intercept":model.intercept}
    a={"artifact_version":artifact_version,"model":m,"calibration":({"method":"none","slope":1.0,"intercept":0.0} if calibration is None else calibration),"model_version":model_version,"feature_schema_hash":feature_schema_hash,"training_data_hash":training_data_hash,"calibration_data_hash":calibration_data_hash,"code_hash":code_hash,"config_hash":config_hash,"created_at":created_at,"artifact_sha256":""}
    _validate({**a,"artifact_sha256":"0"*64},False); a["artifact_sha256"]=sha256(_json({k:v for k,v in a.items() if k!="artifact_sha256"}).encode()).hexdigest(); _validate(a)
    data=(_json(a)+"\n").encode(); p=Path(path); p.parent.mkdir(parents=True,exist_ok=True); fd,tmp=tempfile.mkstemp(prefix="."+p.name+".",dir=p.parent)
    try:
        with os.fdopen(fd,"wb") as f: f.write(data); f.flush(); os.fsync(f.fileno())
        os.replace(tmp,p)
    finally:
        try: os.unlink(tmp)
        except FileNotFoundError: pass
    return a

def load_model(path):
    with open(path,encoding="utf-8") as f:return _validate(json.load(f))
def predict(artifact,features):
    a=load_model(artifact) if isinstance(artifact,(str,Path)) else _validate(artifact); m=a["model"]
    if type(features) is not dict or set(features)!=set(m["feature_names"]): raise ValueError("feature mapping mismatch")
    try: xs=[_num(features[n]) for n in m["feature_names"]]
    except (KeyError, ValueError): raise ValueError("invalid features")
    try:
        z=0 if m["kind"]=="base-rate" else (m["intercept"]+m["learning_rate"]*sum(t["left_value"] if xs[t["feature_index"]] <= t["threshold"] else t["right_value"] for t in m["trees"]) if m["kind"]=="boosted-tree" else m["intercept"]+sum(w*(x-u)/s for w,x,u,s in zip(m["weights"],xs,m["means"],m["scales"])))
    except (OverflowError, ValueError): raise ValueError("nonfinite model output")
    if not math.isfinite(z): raise ValueError("nonfinite model output")
    p=m["rate"] if m["kind"]=="base-rate" else (1/(1+math.exp(-z)) if z>=0 else math.exp(z)/(1+math.exp(z)))
    c=a["calibration"]
    if c["method"]=="none": return p
    q=max(_CLIP,min(1-_CLIP,p)); z=c["intercept"]+c["slope"]*math.log(q/(1-q))
    if not math.isfinite(z): raise ValueError("nonfinite calibrated logit")
    result = 1/(1+math.exp(-z)) if z>=0 else math.exp(z)/(1+math.exp(z))
    if not math.isfinite(result) or not 0 <= result <= 1: raise ValueError("nonfinite probability")
    return result
