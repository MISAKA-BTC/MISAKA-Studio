#!/usr/bin/env bash
# One slot's free-prompt gateway. The port is derived from the slot number so two slots never
# collide, and the identity is the one pool.py wrote from the chain's view of that slot's bond.
set -euo pipefail
SLOT_DIR="${1:?usage: run-fp.sh /var/lib/misaka-minerpool/slots/slot-NN}"
NN="$(basename "$SLOT_DIR" | sed 's/^slot-//')"
PORT=$((18790 + 10#$NN))
export MISAKA_PALW_NETWORK_ID=testnet-11
# The tokenizer-BOUND artifact: the plain qwen25-1.5b-a16.palwart declares an all-zero tokenizer
# commitment and the v5 worker refuses it outright.
export MISAKA_PALW_ARTIFACT=/root/palw-class/bound-candidate.palwart
export MISAKA_PALW_TOKENIZER=/root/palw-class/qwen25-tokenizer.json
export MISAKA_PALW_GATEWAY_LOG_WORKER_STDERR=1
# No --derive-seed and no key of any kind: this process holds none (ADR-0079 Decision 4). The
# slot's seed is the submitter's, one directory away and never passed here.
# **The whole room, because on a slot there are no strangers.** The gateway reserves 200 permille
# of the bond's exposure for "public" jobs by default, so an operator's own claims are not starved
# by callers off the street. Here the caller IS the operator: the pool authenticates every request
# with this slot's token before proxying it in, and the slot's bond exists for exactly these jobs.
# At the default the first job was refused outright — one claim reserves 33,152,720 sompi and 200
# permille of this bond's room is 26,522,176, so the budget was smaller than a single claim and no
# job could ever commit.
exec /root/t11/misaka-palw-gateway \
  --worker /root/t11/palw-a16-fp-worker \
  --outbox "$SLOT_DIR/fp/outbox" \
  --identity "$SLOT_DIR/fp/identity/identity.json" \
  --rpc 127.0.0.1:26313 \
  --class-leaves 6630544 \
  --public-job-budget-permille 1000 \
  --listen "127.0.0.1:$PORT"
