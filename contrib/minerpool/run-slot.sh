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

# --yes: a slot runs under systemd with no tty, and a regenesised testnet asks
# "Genesis not found in active consensus DB ... confirm the delete? (y/n)" on startup. With no
# answer possible the node exits, systemd restarts it, and the slot loops forever while looking
# "activating" — measured on 2026-08-30. Answering yes is correct HERE and only here: a pool slot
# owns nothing but a chain it can re-sync, so deleting a datadir that does not match the network
# is the only outcome an operator would ever pick.
COMMON=(--testnet --netsuffix=11 "--appdir=$SLOT_DIR/appdir" --yes
        "--listen=0.0.0.0:$P2P_PORT" "--rpclisten=127.0.0.1:$GRPC_PORT"
        --nodnsseed --disable-upnp
        --addpeer=127.0.0.1:26311 --addpeer=169.58.39.220:26311 --addpeer=169.58.232.114:26311
        "--palw-producer-key=$SEED" "--palw-producer-pay-address=$ADDRESS")

# **A free-prompt slot's node must be able to OPEN what it retains.** The node keeps the executor's
# capture and answers a seat's interval request from it, and opening one needs the class's artifact
# loaded — without it `resolve_backend` fails and every request is answered "not held", which reads
# on the seat exactly like an executor that is withholding. Measured on testnet-11 with slot-05: a
# 300-token claim retained 183 MB that no seat could ever have been served. Floor slots need none of
# this, so the flag rides only where the slot declares the free-prompt mode.
if [ "$(jread mode)" = "fp" ] && [ -f /root/palw-class/bound-candidate.palwart ]; then
  COMMON+=("--palw-class-artifact=/root/palw-class/bound-candidate.palwart")
fi

# **What this bond will be asked to hold.** kaspad sizes collateral for the FLOOR class, which is
# the right default for a slot that mines the floor and useless for one that runs the free-prompt
# lane: a v5 claim's exposure is several times the floor's, so a floor-sized bond registers fine and
# is then refused every claim, having locked real money to get there. A slot created for the
# free-prompt lane carries the number in its own slot.json, computed from the chain at creation.
COLLATERAL="$(jread bond_collateral)"
COLLATERAL_ARG=()
[ -n "$COLLATERAL" ] && COLLATERAL_ARG=("--palw-bond-collateral=$COLLATERAL")

if [ -z "$BOND" ]; then
  # **Only this run's lines count.** The log outlives the process, so a grep over the whole file
  # reads the PREVIOUS attempt's verdict — and a give-up line from an hour ago restarted the node
  # instantly, forever. The offset is taken before the node starts and every check reads past it.
  LOG_FROM=$(( $(stat -c%s "$LOG" 2>/dev/null || echo 0) + 1 ))
  echo "[pool-slot] phase 1: registering a bond (waiting for funds at $ADDRESS${COLLATERAL:+, collateral $COLLATERAL sompi})" >> "$LOG"
  "$KASPAD" "${COMMON[@]}" "${COLLATERAL_ARG[@]}" --palw-register-bond >> "$LOG" 2>&1 &
  NODE=$!
  trap 'kill -INT $NODE 2>/dev/null; wait $NODE; exit 0' INT TERM

  # The registration line: "[palw-panel] registered bond <txid>:<i> with ... Restart with ..."
  while kill -0 $NODE 2>/dev/null; do
    # **Two sentences mean the same thing.** A first registration prints "registered bond <o>";
    # a node whose carrier was mined while it had already given up prints "this key already holds
    # bond <o> on this chain". Reading only the first left slot-04 restarting forever with its
    # bond sitting on chain, locked and invisible to the runner that was waiting for it.
    BOND=$(tail -c "+$LOG_FROM" "$LOG" \
      | grep -oE '(registered bond|already holds bond) [0-9a-f]+:[0-9]+' | tail -1 | awk '{print $NF}')
    [ -n "$BOND" ] && break
    # **The node gives up before this chain does.** It waits ten minutes for its carrier to appear
    # as a bond and then stops registering; on testnet-11 a block arrives every twenty to forty
    # minutes, so a carrier can be accepted, sit in a mempool, be evicted, and the slot waits
    # forever on a node that has already stopped trying — measured on slot-04, carrier 23d05f62…,
    # which was accepted and never mined. Exiting hands the retry to systemd, which is where a
    # retry belongs: the funds are still there and the next attempt costs a fee, not a bond.
    if tail -c "+$LOG_FROM" "$LOG" | grep -qa 'no bond appeared within'; then
      echo "[pool-slot] the node stopped registering (its carrier was never mined); restarting to try again" >> "$LOG"
      kill -INT $NODE 2>/dev/null
      wait $NODE
      exit 1
    fi
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
