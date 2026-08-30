#!/usr/bin/env bash
# One pool slot = one hosted producer, self-advancing through the join runbook.
#
# Phase 1 (no bond yet): run kaspad with --palw-register-bond. The node itself waits for
# funding at the slot address, sizes the collateral, submits the carrier and waits for the
# bond to appear on chain — all we add is watching its log for the one line where the
# outpoint exists (docs/testnet11-join-mining.md §3: "That line is the only place the
# bond's outpoint appears").
#
# Phase 2 (bond known): exec kaspad with --palw-produce, the carrier's own change as the
# fee outpoint (§4 — without it the panel is receipts-only and a ConsensusV2 producer
# refuses to start), and no --palw-producer-class: omitting it mines the BASE-0 floor,
# which needs no artifact (§5).
#
# kaspad is stopped with SIGINT, not SIGTERM — it only installs a SIGINT handler, and a
# TERM'd node dies without closing RocksDB.
set -u

SLOT_DIR="${1:?usage: run-slot.sh /var/lib/misaka-minerpool/slots/slot-NN}"
SLOT_NN="$(basename "$SLOT_DIR" | sed 's/^slot-//')"
KASPAD=/root/t11/kaspad
SEED="$SLOT_DIR/seed.key"
STATE="$SLOT_DIR/slot.json"
LOG="$SLOT_DIR/kaspad.log"

P2P_PORT=$((17300 + 10#$SLOT_NN))
GRPC_PORT=$((27400 + 10#$SLOT_NN))

jread() { python3 -c "import json,sys;print(json.load(open('$STATE')).get('$1') or '')"; }
jwrite() { python3 - "$STATE" "$1" "$2" <<'PY'
import json,sys
p,k,v=sys.argv[1:4]
d=json.load(open(p)); d[k]=v
json.dump(d,open(p,'w'),indent=1)
PY
}

ADDRESS="$(jread address)"
BOND="$(jread bond_outpoint)"

COMMON=(--testnet --netsuffix=11 "--appdir=$SLOT_DIR/appdir"
        "--listen=0.0.0.0:$P2P_PORT" "--rpclisten=127.0.0.1:$GRPC_PORT"
        --nodnsseed --disable-upnp
        --addpeer=127.0.0.1:26311 --addpeer=169.58.39.220:26311 --addpeer=169.58.232.114:26311
        "--palw-producer-key=$SEED" "--palw-producer-pay-address=$ADDRESS")

if [ -z "$BOND" ]; then
  echo "[pool-slot] phase 1: registering a bond (waiting for funds at $ADDRESS)" >> "$LOG"
  "$KASPAD" "${COMMON[@]}" --palw-register-bond >> "$LOG" 2>&1 &
  NODE=$!
  trap 'kill -INT $NODE 2>/dev/null; wait $NODE; exit 0' INT TERM

  # The registration line: "[palw-panel] registered bond <txid>:<i> with ... Restart with ..."
  while kill -0 $NODE 2>/dev/null; do
    BOND=$(grep -oE 'registered bond [0-9a-f]+:[0-9]+' "$LOG" | tail -1 | awk '{print $3}')
    [ -n "$BOND" ] && break
    sleep 5
  done

  if [ -z "$BOND" ]; then
    # The node exited without registering — leave its last words in the log and let
    # systemd restart us to try again (funding may simply not have arrived yet).
    wait $NODE
    exit 1
  fi

  TXID="${BOND%%:*}"
  jwrite bond_outpoint "$BOND"
  jwrite fee_outpoint "$TXID:1"
  echo "[pool-slot] bond $BOND registered; restarting as a producer" >> "$LOG"
  kill -INT $NODE 2>/dev/null
  wait $NODE
  sleep 2
fi

FEE="$(jread fee_outpoint)"
echo "[pool-slot] phase 2: producing (bond=$BOND fee=$FEE class=floor)" >> "$LOG"
exec "$KASPAD" "${COMMON[@]}" \
  --palw-produce --palw-panel \
  "--palw-producer-bond=$BOND" \
  "--palw-fee-outpoint=$FEE" >> "$LOG" 2>&1
