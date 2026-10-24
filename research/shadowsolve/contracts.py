import json
import re
from pathlib import Path


def load_contract(name, root=None):
    root = Path(root or Path(__file__).parents[2])
    return json.loads((root / "data-contracts" / f"{name}.schema.json").read_text())


def validate(value, schema):
    return _validate(value, schema, schema)


def _validate(value, schema, root):
    if "$ref" in schema:
        target = root
        for part in schema["$ref"].removeprefix("#/").split("/"):
            target = target[part]
        return _validate(value, target, root)
    types = schema.get("type")
    if isinstance(types, list):
        if not any(_validate(value, {**schema, "type": t}, root) for t in types): return False
    elif types == "object":
        if not isinstance(value, dict): return False
        if any(k not in value for k in schema.get("required", [])): return False
        props = schema.get("properties", {})
        if schema.get("additionalProperties") is False and any(k not in props for k in value): return False
        if not all(_validate(v, props[k], root) for k, v in value.items() if k in props): return False
    elif types == "array":
        if not isinstance(value, list) or (schema.get("uniqueItems") and len({json.dumps(x, sort_keys=True) for x in value}) != len(value)): return False
        if not all(_validate(x, schema.get("items", {}), root) for x in value): return False
    elif types == "string":
        if not isinstance(value, str): return False
        if len(value) < schema.get("minLength", 0) or len(value) > schema.get("maxLength", float("inf")): return False
        if "pattern" in schema and re.fullmatch(schema["pattern"], value) is None: return False
    elif types == "integer":
        if not isinstance(value, int) or isinstance(value, bool): return False
    elif types == "number":
        if not isinstance(value, (int, float)) or isinstance(value, bool): return False
    elif types == "null":
        return value is None
    elif types == "boolean" and not isinstance(value, bool): return False
    if "minimum" in schema and (not isinstance(value, (int, float)) or value < schema["minimum"]): return False
    if "maximum" in schema and (not isinstance(value, (int, float)) or value > schema["maximum"]): return False
    if "const" in schema and value != schema["const"]: return False
    if "enum" in schema and value not in schema["enum"]: return False
    return True
