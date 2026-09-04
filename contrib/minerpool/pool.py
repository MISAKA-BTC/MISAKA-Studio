#!/usr/bin/env python3
"""The MISAKA miner pool — hosted producer slots, joined with nothing but funds.

What this is: a machine that already runs a synced testnet-11 node rents out
*producer slots*. A slot is a real `kaspad --palw-produce` with its own ML-DSA-87
seed, its own bond, and its own appdir, supervised by systemd
(`misaka-pool-slot@NN`). Joining creates the slot and hands the caller the slot's
address and seed; funding that address is the only thing left to do — the slot
node itself waits for the funds, registers the bond (sizing collateral the way
docs/testnet11-join-mining.md §3 describes), and flips to producing.

What this is NOT: a work-splitting pool. On this network the thing that runs the
model is the thing that makes the block, and it must hold the bonded key — so a
"mine without a node" offer is precisely "someone else runs your producer".
This service says so instead of hiding it: the slot seed is generated here,
STAYS here, and is also returned once to the joiner, who is told rewards accrue
at the slot address and that this host can spend them too. On a test network
that trade is the product; on mainnet it would be a custody business.

The API is deliberately small and stateless: every answer is derived from the
slot directory (slot.json + the node's own log), because the log is what the
node actually said — a status field we maintained separately would drift into
flattery.
"""

import fcntl
import json
import os
import re
import secrets
import subprocess
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = "/var/lib/misaka-minerpool"
SLOTS = os.path.join(ROOT, "slots")
MISAKA = "/root/misaka"
NETWORK = "testnet-11"
MAX_SLOTS = 6
# 10 MSK: the runbook's worked example. The chain's own floor is lower, but the
# KIP-0009 storage-mass relay limit puts the smallest carryable collateral near
# 8.34M sompi when funded with 10 MSK — below ~5 MSK no split of the funding
# clears the limit and the node will sit telling you to send more.
MIN_FUNDING_SOMPI = 1_000_000_000
JOIN_COOLDOWN_S = 60

_last_join = [0.0]


def slot_dir(slot_id):
    if not re.fullmatch(r"slot-[0-9]{2}", slot_id):
        return None
    d = os.path.join(SLOTS, slot_id)
    return d if os.path.isdir(d) else None


def read_state(d):
    with open(os.path.join(d, "slot.json")) as f:
        return json.load(f)


def unit_active(slot_id):
    r = subprocess.run(
        ["systemctl", "is-active", f"misaka-pool-slot@{slot_id[5:]}"],
        capture_output=True, text=True, timeout=10,
    )
    return r.stdout.strip() == "active"


def tail(path, lines=400):
    try:
        with open(path, "rb") as f:
            f.seek(0, 2)
            f.seek(max(0, f.tell() - 256 * 1024))
            return f.read().decode("utf-8", "replace").splitlines()[-lines:]
    except OSError:
        return []


def derive_status(d, state, active):
    """The phase, read from what the node last said — not from what we hoped."""
    log = tail(os.path.join(d, "kaspad.log"))
    palw = [l for l in log if "palw" in l.lower() or "pool-slot" in l]
    recent = palw[-15:]

    if state.get("bond_outpoint"):
        phase = "bonded"
        for l in reversed(palw):
            if "holding" in l:
                phase = "holding"
                break
            # **"producing" must mean blocks, not a thread that started.** `[palw-producer]` also
            # matches the producer's own "starting (bond=…)" line, so a slot that had drawn nothing
            # for half an hour reported `producing` to the Studio, which showed it to a person as
            # mining (measured 2026-09-04 on slot-02: producer up, ~5 % CPU, zero draws logged —
            # the producer holds SILENTLY at trace level when the mining rule engine says no).
            if "produced block" in l or "produced RECEIPT" in l:
                phase = "producing"
                break
            if "phase 2: producing" in l or "[palw-producer] starting" in l:
                phase = "drawing"
                break
        if not active:
            phase = "stopped"
    else:
        phase = "starting"
        for l in reversed(palw):
            if "cannot register a bond yet" in l or "no confirmed UTXO" in l:
                phase = "awaiting_funds"
                break
            if "registered bond" in l:
                phase = "registering"
                break
        if not active:
            phase = "stopped"

    produced = sum(1 for l in log if "produced block" in l)
    return phase, recent, produced


def balance_sompi(address):
    """Confirmed UTXO total at an address, asked of the shared node. Best effort:
    a pool whose status endpoint dies when the wallet RPC hiccups is worse than
    one that omits the number."""
    try:
        r = subprocess.run(
            [MISAKA, "--network", NETWORK, "--rpc", "127.0.0.1:26313", "--output", "json",
             "wallet", "utxo", "list", "--address", address],
            capture_output=True, text=True, timeout=15,
        )
        if r.returncode != 0:
            return None
        data = json.loads(r.stdout)
        return int((data.get("mature") or {}).get("sompi") or 0)
    except Exception:
        return None


def create_slot():
    os.makedirs(SLOTS, exist_ok=True)
    with open(os.path.join(ROOT, "lock"), "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        used = sorted(x for x in os.listdir(SLOTS) if re.fullmatch(r"slot-[0-9]{2}", x))
        if len(used) >= MAX_SLOTS:
            return None, "the pool is full — every slot is taken"
        nn = next(f"{i:02d}" for i in range(1, MAX_SLOTS + 1) if f"slot-{i:02d}" not in used)
        slot_id = f"slot-{nn}"
        d = os.path.join(SLOTS, slot_id)
        os.makedirs(os.path.join(d, "appdir"), exist_ok=True)

        seed_path = os.path.join(d, "seed.key")
        gen = subprocess.run(
            [MISAKA, "--network", NETWORK, "key", "gen", "--out", seed_path],
            capture_output=True, text=True, timeout=30,
        )
        m = re.search(r"(misakatest:[a-z0-9]+)", gen.stdout + gen.stderr)
        if gen.returncode != 0 or not m:
            return None, f"key generation failed: {(gen.stderr or gen.stdout)[:300]}"
        address = m.group(1)
        with open(seed_path) as f:
            seed_hex = f.read().strip()

        state = {
            "slot_id": slot_id,
            "token": secrets.token_hex(16),
            "address": address,
            "created_unix": int(time.time()),
        }
        with open(os.path.join(d, "slot.json"), "w") as f:
            json.dump(state, f, indent=1)

        subprocess.run(["systemctl", "enable", "--now", f"misaka-pool-slot@{nn}"],
                       capture_output=True, timeout=30)
        return {**state, "seed_hex": seed_hex}, None


class Handler(BaseHTTPRequestHandler):
    server_version = "misaka-minerpool/1"

    def _json(self, code, obj):
        body = json.dumps(obj, indent=1).encode()
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass

    def do_GET(self):
        if self.path == "/pool/v1/info":
            used = sorted(x for x in os.listdir(SLOTS) if re.fullmatch(r"slot-[0-9]{2}", x)) \
                if os.path.isdir(SLOTS) else []
            return self._json(200, {
                "network": NETWORK,
                "class": "PALW-BASE-0 (the integer floor — no model file, always producible)",
                "slots_total": MAX_SLOTS,
                "slots_used": len(used),
                "min_funding_sompi": MIN_FUNDING_SOMPI,
                "custody": "the slot's producer seed is generated on this host and kept here; "
                           "it is also returned once at join. Rewards accrue at the slot address, "
                           "which that seed controls.",
            })

        m = re.fullmatch(r"/pool/v1/slots/(slot-[0-9]{2})", self.path.split("?")[0])
        if m:
            d = slot_dir(m.group(1))
            if not d:
                return self._json(404, {"error": "no such slot"})
            state = read_state(d)
            token = self.headers.get("x-pool-token") or \
                (re.search(r"[?&]token=([0-9a-f]+)", self.path) or [None, ""])[1]
            if token != state["token"]:
                return self._json(403, {"error": "wrong or missing slot token"})
            active = unit_active(state["slot_id"])
            phase, activity, produced = derive_status(d, state, active)
            return self._json(200, {
                "slot_id": state["slot_id"],
                "address": state["address"],
                "phase": phase,
                "bond_outpoint": state.get("bond_outpoint"),
                "fee_outpoint": state.get("fee_outpoint"),
                "balance_sompi": balance_sompi(state["address"]),
                "min_funding_sompi": MIN_FUNDING_SOMPI,
                "blocks_won": produced,
                "activity": activity,
            })
        return self._json(404, {"error": "unknown path"})

    def do_POST(self):
        if self.path != "/pool/v1/slots":
            return self._json(404, {"error": "unknown path"})
        now = time.time()
        if now - _last_join[0] < JOIN_COOLDOWN_S:
            return self._json(429, {"error": "a slot was just created — try again in a minute"})
        slot, err = create_slot()
        if err:
            return self._json(409, {"error": err})
        _last_join[0] = now
        return self._json(201, {
            **slot,
            "min_funding_sompi": MIN_FUNDING_SOMPI,
            "next_step": f"send at least {MIN_FUNDING_SOMPI} sompi (10 MSK), in a normal transfer "
                         f"(not mining rewards), to {slot['address']} — the slot registers its "
                         "bond by itself and starts mining",
            "custody": "this seed also stays on the pool host — that is what running your "
                       "producer for you means. Keep your copy; it controls the rewards.",
        })


if __name__ == "__main__":
    os.makedirs(SLOTS, exist_ok=True)
    ThreadingHTTPServer(("127.0.0.1", 8799), Handler).serve_forever()
