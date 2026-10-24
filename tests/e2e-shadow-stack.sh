#!/usr/bin/env bash
set -Eeuo pipefail
base=${BASE_URL:-http://localhost:${DASHBOARD_HOST_PORT:-5173}}
compose=(docker compose --profile e2e)
fail(){ printf 'E2E failed: %s\n' "$1" >&2; exit 1; }
command -v curl >/dev/null || fail 'curl is required'
command -v python3 >/dev/null || fail 'python3 is required'
command -v docker >/dev/null || fail 'docker is required'
curl=(curl --connect-timeout 2 --max-time 10)
if [[ ${ALLOW_NON_LOOPBACK_BASE_URL:-0} != 1 && ! $base =~ ^https?://(localhost|127\.0\.0\.1|\[::1\])(:[0-9]+)?(/|$) ]]; then fail 'BASE_URL must target localhost or loopback'; fi
result=$(mktemp)
run_id="e2e-$(date +%s)-$$"
auction_id="${run_id}"
export AUCTION_ID="$auction_id"
restore(){ local status=$?; if ! env -u SHADOWSOLVE_DASHBOARD_MODE -u SHADOWSOLVE_MODEL_PATH -u SHADOWSOLVE_PROTOCOL_POLICY_PATH -u MODEL_ARTIFACT_HOST_PATH -u SHADOWSOLVE_RPC_URL -u SHADOWSOLVE_INGESTION_ENABLED -u SHADOWSOLVE_INGESTION_POOLS -u SHADOWSOLVE_FINALITY_DEPTH -u SHADOWSOLVE_POLL_SECONDS -u SHADOWSOLVE_MAX_AGE_SECONDS "${compose[@]}" up -d --force-recreate >/dev/null 2>&1; then printf "E2E cleanup failed: unable to restore compose services\n" >&2; fi; rm -f "$result"; return "$status"; }
trap restore EXIT
json_assert(){ python3 - "$@"; }
get(){ "${curl[@]}" -fsS "$base$1"; }
metric_value(){ awk -v name="$1" '$1==name && NF>=2 {print $2; found=1} END{if(!found) print 0}' <<<"$2"; }
status(){ local path=$1; shift; "${curl[@]}" -sS -o /dev/null -w '%{http_code}' "$@" "$base$path"; }
wait_for(){ for _ in $(seq 1 30); do get "$1" >/dev/null 2>&1 && return; sleep 1; done; fail "timeout waiting for $1"; }
solver_base=http://localhost:${SOLVER_HOST_PORT:-8080}
# Avoid colliding with unrelated local listeners on :8080 (Compose publishes SOLVER_HOST_PORT).
if [[ -z ${SOLVER_HOST_PORT:-} ]] && lsof -nP -iTCP:8080 -sTCP:LISTEN 2>/dev/null | grep -Eqv 'com\.docker|Docker|orb|vpnkit'; then
  export SOLVER_HOST_PORT=18080
  solver_base=http://localhost:18080
fi
printf 'E2E solver_base=%s\n' "$solver_base"
rpc_mode(){ "${compose[@]}" exec -T mock-rpc python3 - "$1" <<'PY'
import sys, urllib.request
urllib.request.urlopen('http://127.0.0.1:18545/mode/' + sys.argv[1])
PY
}
# Fresh volumes avoid immutable snapshot conflicts across local re-runs.
"${compose[@]}" down -v >/dev/null 2>&1 || true
"${compose[@]}" up -d mock-rpc >/dev/null
# Ingestion is opt-in; enable it explicitly for the fail-closed freshness/reorg path.
export SHADOWSOLVE_INGESTION_ENABLED=true
export SHADOWSOLVE_RPC_URL="http://mock-rpc:18545"
export SHADOWSOLVE_INGESTION_POOLS=0x0000000000000000000000000000000000000001
export SHADOWSOLVE_FINALITY_DEPTH=2 SHADOWSOLVE_POLL_SECONDS=1 SHADOWSOLVE_MAX_AGE_SECONDS=300
# Keep the first configured path free of a missing host model artifact from .env.
export SHADOWSOLVE_MODEL_PATH=
export SHADOWSOLVE_PROTOCOL_POLICY_PATH=
# Recreate first so mock-rpc mode is not wiped by --force-recreate.
"${compose[@]}" up -d --force-recreate >/dev/null
for _ in $(seq 1 60); do "${curl[@]}" -fsS "$solver_base/health" >/dev/null 2>&1 && break; sleep 1; done
rpc_mode ready
for _ in $(seq 1 60); do [[ $("${curl[@]}" -sS -o /dev/null -w '%{http_code}' "$solver_base/ready") == 200 ]] && break; sleep 1; done
[[ $("${curl[@]}" -sS -o /dev/null -w '%{http_code}' "$solver_base/ready") == 200 ]] || fail 'ingestion did not become ready before stale probe'
rpc_mode stale
sleep 2
for _ in $(seq 1 30); do [[ $("${curl[@]}" -sS -o /dev/null -w '%{http_code}' "$solver_base/ready" || true) == 503 ]] && break; sleep 1; done
[[ $("${curl[@]}" -sS -o /dev/null -w '%{http_code}' "$solver_base/ready" || true) == 503 ]] || fail "stale ingestion did not close readiness ($( "${curl[@]}" -sS "$solver_base/ready" || true))"
rpc_mode ready
sleep 2
for _ in $(seq 1 30); do [[ $("${curl[@]}" -sS -o /dev/null -w '%{http_code}' "$solver_base/ready" || true) == 200 ]] && break; sleep 1; done
wait_for /health; wait_for /ready
health=$(get /health); [[ $health == ok ]] || fail 'health payload'
ready=$(get /ready); json_assert "$ready" <<'PY'
import json,sys,os
v=json.loads(sys.argv[1]); assert v['ready'] is True and v['persistenceMode']=='postgres'
assert v['projectionInitialized'] is True and v['notificationRepositoryInitialized'] is True
assert v['modelAvailable'] is False and v['modelFallback'] is True and v['modelFallbackReason']=='no_model_configured'
assert v['ingestion']['fresh'] is True and v['ingestion']['canonical'] is True and v['ingestion']['finalized']==18
PY
baseline_metrics=$(get /metrics)
base_solves=$(metric_value shadowsolve_solves_total "$baseline_metrics")
base_accepted=$(metric_value shadowsolve_notifications_accepted_total "$baseline_metrics")
base_duplicate=$(metric_value shadowsolve_notifications_duplicate_total "$baseline_metrics")
future=$(python3 -c 'import time; print(int(time.time()*1000)+600000)')
request=$(python3 - "$future" <<'PY'
import json,sys,os
print(json.dumps({'schema_version':'1','auction':{'id':os.environ["AUCTION_ID"],'block':18,'tokens':['WETH','USDC'],'orders':[{'id':'order-1','sell_token':'WETH','buy_token':'USDC','sell_amount':'10000','min_buy_amount':'1','kind':'sell'}],'state':{'observed_block':18,'deadline_ms':int(sys.argv[1])}},'pools':[{'id':'0x0000000000000000000000000000000000000001','token0':'WETH','token1':'USDC','reserve0':'1','reserve1':'1','fee_bps':30,'concentrated':False}],'deadline_ms':int(sys.argv[1])}))
PY
)
code=$("${curl[@]}" -sS -o "$result" -w '%{http_code}' -H 'Content-Type: application/json' -X POST "$base/solve" -d "$request"); [[ $code == 200 ]] || fail "solve returned $code"
json_assert "$(<"$result")" <<'PY'
import json,sys,os
v=json.loads(sys.argv[1]); assert v['auction_id']==os.environ["AUCTION_ID"] and v['candidates']; assert all(c['auction_id']==os.environ["AUCTION_ID"] for c in v['candidates'])
assert all(c.get('routes') for c in v['candidates'])
p=v['provenance']; assert p['verification']=='canonical_finalized_rpc' and p['source_block_number']==18
assert p['source_block_hash'].startswith('0xa') and p['source_network']=='ethereum-mainnet'
PY
notification=$(python3 -c 'import json, sys; print(json.dumps({"schema_version":"1","auction_id":sys.argv[1],"outcome":"accepted","candidate_id":None}))' "$auction_id")
code=$("${curl[@]}" -sS -o "$result" -w '%{http_code}' -H 'Content-Type: application/json' -X POST "$base/notify" -d "$notification"); [[ $code == 202 ]] || fail "first notify returned $code"
json_assert "$(<"$result")" <<'PY'
import json,sys,os
v=json.loads(sys.argv[1]); assert v['accepted'] is True and v['duplicate'] is False and v['notification']['auction_id']==os.environ["AUCTION_ID"]
PY
code=$("${curl[@]}" -sS -o "$result" -w '%{http_code}' -H 'Content-Type: application/json' -X POST "$base/notify" -d "$notification"); [[ $code == 200 ]] || fail "duplicate notify returned $code"
json_assert "$(<"$result")" <<'PY'
import json,sys,os
v=json.loads(sys.argv[1]); assert v['accepted'] is False and v['duplicate'] is True
PY
psql_count(){ "${compose[@]}" exec -T postgres psql -U "${POSTGRES_USER:-shadowsolve_dev}" -d "${POSTGRES_DB:-shadowsolve}" -Atqc "$1" | tr -d '[:space:]'; }
	[[ $(psql_count "select count(*) from auctions where id='${auction_id}';") == 1 ]] || fail 'auction row missing'
	[[ $(psql_count "select count(*) from candidates where auction_id='${auction_id}';") -ge 1 ]] || fail 'candidate rows missing'
	[[ $(psql_count "select count(*) from predictions where auction_id='${auction_id}';") -ge 1 ]] || fail 'prediction rows missing'
	[[ $(psql_count "select count(*) from advisory_provenance where auction_id='${auction_id}' and source_network='ethereum-mainnet' and block_number=18;") == 1 ]] || fail 'advisory provenance row missing'
	[[ $(psql_count "select count(*) from notifications where payload->>'schema_version'='1' and payload->>'auction_id'='${auction_id}' and payload->>'candidate_id' is null;") == 1 ]] || fail 'notification payload row count'
metrics=$(get /metrics)
[[ $(metric_value shadowsolve_solves_total "$metrics") -eq $((base_solves+1)) ]] || fail 'solve metric delta missing'
[[ $(metric_value shadowsolve_notifications_accepted_total "$metrics") -eq $((base_accepted+1)) ]] || fail 'accepted metric delta missing'
[[ $(metric_value shadowsolve_notifications_duplicate_total "$metrics") -eq $((base_duplicate+1)) ]] || fail 'duplicate metric delta missing'
grep -q '^shadowsolve_dashboard_revision ' <<<"$metrics" || fail 'revision metric missing'
grep -q '^shadowsolve_dashboard_update_count ' <<<"$metrics" || fail 'delta metric missing'
grep -q '^shadowsolve_ingestion_fresh 1' <<<"$metrics" || fail 'ingestion freshness metric missing'
grep -q '^shadowsolve_ingestion_info{network="ethereum-mainnet",hash="0xa' <<<"$metrics" || fail 'ingestion hash metric missing'
grep -q '^shadowsolve_latest_advisory_info{.*network="ethereum-mainnet".*block_hash="0xa.*} 18' <<<"$metrics" || fail 'latest advisory metric missing'
dashboard=$(get /api/dashboard); json_assert "$dashboard" <<'PY'
import json,sys,os
v=json.loads(sys.argv[1]); assert v['meta']['revision']>=2 and v['meta']['source']=='live'
assert v['meta']['chain']['fresh'] is True and v['meta']['chain']['finalized']==18
assert any(x.get('id')==os.environ["AUCTION_ID"] for x in v['auctions']); assert v['candidates']; assert len(v['notifications'])==1
assert v['latestAdvisory']['source_block_number']==18
PY
"${curl[@]}" -fsS "$base/" | grep -q 'ShadowSolve' || fail 'dashboard not served'
"${curl[@]}" -fsS "$base/?fixture=1" | grep -q 'ShadowSolve' || fail 'fixture browser marker missing'
for route in /health /ready /metrics /api/dashboard; do [[ $(status "$route" -X POST) == 405 ]] || fail "POST method guard failed for $route"; done
for route in /solve /notify; do [[ $(status "$route" -X GET) == 405 ]] || fail "GET method guard failed for $route"; done
# Restart exercises PostgreSQL reconstruction and notification dedupe persistence.
"${compose[@]}" restart solver >/dev/null; wait_for /ready
restarted=$(get /api/dashboard); json_assert "$restarted" <<'PY'
import json,sys,os
v=json.loads(sys.argv[1]); assert any(x.get('id')==os.environ["AUCTION_ID"] for x in v['auctions']); assert v['candidates']; assert len(v['notifications'])==1
assert v['latestAdvisory']['source_block_number']==18
PY
# A finalized reorg is unsafe until restart rebuilds the bounded canonical chain.
rpc_mode reorg
for _ in $(seq 1 15); do [[ $("${curl[@]}" -sS -o /dev/null -w '%{http_code}' "$solver_base/ready") == 503 ]] && break; sleep 1; done
unsafe_ready=$("${curl[@]}" -sS "$solver_base/ready"); grep -q '"unsafe":true' <<<"$unsafe_ready" || fail 'reorg did not mark ingestion unsafe'
[[ $("${curl[@]}" -sS -o "$result" -w '%{http_code}' -H 'Content-Type: application/json' -X POST "$solver_base/solve" -d "$request") == 503 ]] || fail 'unsafe ingestion did not close solve'
rpc_mode ready
"${compose[@]}" up -d --force-recreate solver >/dev/null
for _ in $(seq 1 90); do
  code=$("${curl[@]}" -sS -o /tmp/shadowsolve-ready.json -w '%{http_code}' "$solver_base/ready" || true)
  [[ $code == 200 ]] && break
  sleep 1
done
[[ $("${curl[@]}" -sS -o /dev/null -w '%{http_code}' "$solver_base/ready") == 200 ]] || fail "post-reorg readiness did not recover: $(cat /tmp/shadowsolve-ready.json 2>/dev/null || true)"
wait_for /health; wait_for /ready
# Fixture mode must not mutate the live database.
if [[ ${RUN_FIXTURE_SUBTEST:-1} == 1 ]]; then
  before_auctions=$(psql_count 'select count(*) from auctions;'); before_notifications=$(psql_count 'select count(*) from notifications;')
  export SHADOWSOLVE_DASHBOARD_MODE=fixture
  export SHADOWSOLVE_INGESTION_ENABLED=false
  "${compose[@]}" up -d --force-recreate >/dev/null 2>&1 || fail 'fixture compose recreate failed'
  wait_for /ready; fixture_dashboard=$(get /api/dashboard); grep -q '"source":"fixture"' <<<"$fixture_dashboard" || fail 'fixture mode marker missing'
  fixture_code=$("${curl[@]}" -sS -o "$result" -w '%{http_code}' -H 'Content-Type: application/json' -X POST "$base/solve" -d "$request"); [[ $fixture_code == 200 ]] || fail 'fixture solve failed'
  fixture_code=$("${curl[@]}" -sS -o "$result" -w '%{http_code}' -H 'Content-Type: application/json' -X POST "$base/notify" -d "$notification"); [[ $fixture_code == 202 || $fixture_code == 200 ]] || fail 'fixture notify failed'
  [[ $(psql_count 'select count(*) from auctions;') == "$before_auctions" ]] || fail 'fixture mode changed auctions'
  [[ $(psql_count 'select count(*) from notifications;') == "$before_notifications" ]] || fail 'fixture mode changed notifications'
  export SHADOWSOLVE_DASHBOARD_MODE=live
  "${compose[@]}" up -d --force-recreate >/dev/null
fi
# Optional configured-mode checks run only when Docker can recreate services.
if [[ ${RUN_CONFIGURED_SUBTESTS:-1} == 1 ]]; then
  export SHADOWSOLVE_INGESTION_ENABLED=false
  unset SHADOWSOLVE_RPC_URL SHADOWSOLVE_INGESTION_POOLS
  export SHADOWSOLVE_MODEL_PATH=/models/online-model.json SHADOWSOLVE_PROTOCOL_POLICY_PATH=/test-fixtures/protocol-policy.json
  export MODEL_ARTIFACT_HOST_PATH="$PWD/fixtures"
  "${compose[@]}" up -d --force-recreate >/dev/null 2>&1 || fail 'configured compose recreate failed'
  wait_for /ready; configured=$(get /ready); grep -q '"modelAvailable":true' <<<"$configured" || fail 'online model not configured'; grep -q '"protocolPolicyConfigured":true' <<<"$configured" || fail 'policy not configured'
  bad='{"schema_version":"1","auction":{"id":"bad-symbolic","block":1,"tokens":["BAD","USDC"],"orders":[{"id":"bad-order","sell_token":"BAD","buy_token":"USDC","sell_amount":"100","min_buy_amount":"1","kind":"sell"}],"state":{"observed_block":1,"deadline_ms":9999999999999}},"pools":[{"id":"bad-pool","token0":"BAD","token1":"USDC","reserve0":"100000","reserve1":"100000","fee_bps":30}],"deadline_ms":9999999999999}'
  bad_code=$("${curl[@]}" -sS -o "$result" -w '%{http_code}' -H 'Content-Type: application/json' -X POST "$base/solve" -d "$bad")
  if [[ $bad_code == 200 ]]; then json_assert "$(<"$result")" <<'PY'
import json,sys,os
v=json.loads(sys.argv[1]); assert not v.get('candidates'), 'policy accepted disallowed symbolic input'
PY
  else json_assert "$(<"$result")" <<'PY'
import json,sys,os
v=json.loads(sys.argv[1]); assert isinstance(v.get('error'),str) and v['error'], 'unstructured policy rejection'
PY
  fi
  unset SHADOWSOLVE_MODEL_PATH SHADOWSOLVE_PROTOCOL_POLICY_PATH MODEL_ARTIFACT_HOST_PATH
  "${compose[@]}" up -d --force-recreate >/dev/null
fi
printf 'E2E passed: RPC freshness/reorg fail-closed behavior, advisory provenance/restart/dashboard, Postgres solve rows, notification dedupe, metrics, fixture marker, configured model/policy, and method guards.\n'
