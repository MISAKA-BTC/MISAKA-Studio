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

# **A directory is not a tenant.** The pool used to count `slot-NN` directories, so two slots an
# operator stopped on 2026-09-05 (never bonded, 0 sompi, units dead) held a third of the capacity
# for a week and every join was told "the pool is full" while four nodes ran. A slot now holds its
# number only while it could still matter to someone: it runs, it holds a bond, it holds funds, or it
# is young enough that its owner may still be sending them. A reclaimed slot is MOVED to `archived/`
# — never deleted, because its seed controls an address and that is custody, not clutter.
SLOT_GRACE_S = 24 * 3600
ARCHIVE = os.path.join(SLOTS, "archived")
# **What the host must still have before it starts another node.** The slot count was the only
# gate, and on this host (24 GB) it is the wrong one: the explorer node was OOM-killed on 2026-09-05
# with two panel seats and two slots up. A floor slot is a whole kaspad (its own database, ~0.4 GB
# resident once synced, more while syncing); a free-prompt slot also maps the 1.7 GB class artifact
# and gets a gateway capped at 3 GB. Read from MemAvailable — the kernel's own estimate of what can
# be allocated without swapping — so the answer is the host's, not a number we remembered.
MEM_NEEDED_BYTES = {"floor": 3 * 2**30, "fp": 8 * 2**30}

# ---------------------------------------------------------------------------------------------
# The free-prompt lane, as a slot can hold it
# ---------------------------------------------------------------------------------------------
#
# A floor slot mines the lottery: the job is derived from the block's own position and nobody
# chooses it. A free-prompt slot mines what its owner TYPED — the same execution answers them and
# commits the claim (ADR-0044) — and that needs three things a floor slot does not have:
#
#   1. a bond big enough. kaspad sizes collateral for the floor class, and a v5 claim's exposure is
#      several times that, so a floor-sized bond registers and is then refused every claim. The
#      number is computed from the chain at creation and written into slot.json, where run-slot.sh
#      reads it; it cannot be fixed afterwards, because a node registers ONE bond.
#   2. a gateway holding the class artifact, running under THIS slot's bond identity. One per slot:
#      the identity is baked in at startup, so a shared gateway could not say whose work a job is.
#   3. a submitter holding the slot's own seed. The gateway holds no key at all (ADR-0079 D4), so
#      something else has to sign the claim and pay for its carrier — and it must run here, beside
#      the node whose retention directory serves the claim's payload to the panel.
FP_CLASS_ID = ("4277d84f7d91528cc04aa366d51ee1c2e4f7902c4f6b16a213dead1c7e227977"
               "db732f18ed6183db3d944d44726ebd3feff7b15c48f9dba11cd526684f35f1b7")
FP_CLASS_NAME = "PALW-QWEN25-A16 (Qwen2.5-1.5B, graph-v5@512)"
# Exposure per claim is `pwu * slash_value_per_pwu`, and a bond may hold claims up to
# `collateral * max_exposure_ratio_permille / 1000`. Measured against a registered bond on this
# chain: 33,152,720 exposure at pwu 6,630,544 (×5) with a ceiling of half the collateral (500‰),
# so one claim needs `pwu * 10` locked. Both constants are re-read from a live bond when one is
# reachable; these are the fallback, and they are stated rather than hidden so a chain that moves
# them is a number that visibly disagrees.
FP_SLASH_VALUE_PER_PWU = 5
FP_EXPOSURE_RATIO_PERMILLE = 500
# Claims stay open through bind, receipt and challenge windows, so several overlap even though the
# gateway runs one job at a time.
FP_CONCURRENT_CLAIMS = 8
FP_GATEWAY_PORT_BASE = 18790
NODE_WRPC = "127.0.0.1:26314"

_last_join = [0.0]


def _wrpc(method, params):
    import sys
    sys.path.insert(0, "/opt/misaka-minerpool")
    from wrpc import WsRpc
    host, port = NODE_WRPC.split(":")
    return WsRpc(port=int(port)).call(method, params)


def fp_facts(bond_outpoint=None):
    """What the chain says about the free-prompt class, and about one bond under it."""
    txid, index = ("", 0)
    if bond_outpoint:
        txid, index = bond_outpoint.split(":")
    return _wrpc("getPalwProducerFacts", {
        "classId": FP_CLASS_ID,
        "bondTransactionId": txid,
        "bondIndex": int(index),
        "withBond": bool(bond_outpoint),
    })


def fp_collateral_sompi():
    """**What a free-prompt slot has to lock, asked of the chain rather than remembered.**

    Returns `(collateral, why)`. `why` is the arithmetic, verbatim, because this number decides
    how much a joiner has to send and a wrong one is money locked for nothing.
    """
    try:
        facts = fp_facts()
        pwu = int(facts.get("pwu") or 0)
    except Exception as e:
        return None, f"the chain could not be asked: {e}"
    if pwu <= 0:
        return None, "the chain reports no pwu for this class"
    per_claim = pwu * FP_SLASH_VALUE_PER_PWU
    one = -(-per_claim * 1000 // FP_EXPOSURE_RATIO_PERMILLE)   # ceil
    total = one * FP_CONCURRENT_CLAIMS
    why = (f"pwu {pwu} x slash {FP_SLASH_VALUE_PER_PWU} = {per_claim} exposure per claim; "
           f"a bond may hold {FP_EXPOSURE_RATIO_PERMILLE}/1000 of its collateral, so one claim "
           f"needs {one} sompi locked, and {FP_CONCURRENT_CLAIMS} overlapping claims need {total}")
    return total, why


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


def tail(path, lines=400, window_bytes=256 * 1024):
    """The end of a log, as lines. `window_bytes` has to be asked for with `lines`: seeking a fixed
    256 KB back and then taking the last N lines caps the reach at whichever is smaller."""
    try:
        with open(path, "rb") as f:
            f.seek(0, 2)
            f.seek(max(0, f.tell() - window_bytes))
            return f.read().decode("utf-8", "replace").splitlines()[-lines:]
    except OSError:
        return []


def derive_status(d, state, active):
    """The phase, read from what the node last said — not from what we hoped."""
    # **A noisy log must not hide the phase.** The line that says a slot is waiting for funds
    # repeats once a minute; a syncing node emits `recovery-trace` lines faster than that, and in a
    # 400-line window there were NONE of the first left — so a slot that had been asking for money
    # for minutes reported "starting", and the panel never offered the faucet button that is the
    # whole point of that phase. Measured on slot-06. The window is the phase's, not the display's:
    # `recent` below still shows the last fifteen.
    log = tail(os.path.join(d, "kaspad.log"), lines=20_000, window_bytes=8 * 1024 * 1024)
    palw = [l for l in log if "palw" in l.lower() or "pool-slot" in l]
    recent = palw[-15:]

    if state.get("bond_outpoint"):
        phase = "bonded"
        for l in reversed(palw):
            if "holding" in l:
                phase = "holding"
                break
            if "phase 2: producing" in l or "[palw-producer]" in l:
                phase = "producing"
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


RULE_CACHE = os.path.join(ROOT, "coinbase-rule.json")


def _coinbase_rule():
    """(base maturity, long settlement) in DAA, from the node's own tooling.

    The CLI prints them only when something is immature BY ITS OWN reckoning, so the pair is
    cached the first time it is seen. Returns (None, None) when they have never been observed —
    the caller then reports no rule at all, because a wrong constant here would mislabel real
    money as locked."""
    try:
        with open(RULE_CACHE) as f:
            c = json.load(f)
            if c.get("maturity") is not None:
                return int(c["maturity"]), int(c["settlement"])
    except Exception:
        pass
    return None, None


def _learn_coinbase_rule(address):
    """Try to observe the constants; harmless and quiet when the CLI says nothing."""
    try:
        r = subprocess.run(
            [MISAKA, "--network", NETWORK, "--rpc", "127.0.0.1:26313",
             "wallet", "utxo", "list", "--address", address],
            capture_output=True, text=True, timeout=15,
        )
        m = re.search(r"maturity (\d+) \+ settlement (\d+)", r.stdout or "")
        if m:
            with open(RULE_CACHE, "w") as f:
                json.dump({"maturity": int(m.group(1)), "settlement": int(m.group(2)),
                           "seen_unix": int(time.time())}, f)
            return int(m.group(1)), int(m.group(2))
    except Exception:
        pass
    return None, None


def _spendable(is_coinbase, block_daa, virtual_daa, maturity, settlement, anchor_daa):
    """`coinbase_spend_settled`, transcribed. Kept a separate function so it can be read against
    the Rust it mirrors line for line."""
    if not is_coinbase:
        return True
    if virtual_daa < block_daa:
        return False
    age = virtual_daa - block_daa
    if age < maturity:
        return False
    if not settlement:
        return True
    if age >= settlement:
        return True
    return anchor_daa is not None and anchor_daa >= block_daa


def funds_breakdown(address):
    """What this address holds, split by what the node would actually let it spend.

    `None` when the shared node cannot be asked — a status endpoint that dies with the wallet RPC
    is worse than one that omits the number."""
    try:
        info = _wrpc("getBlockDagInfo", {})
        virtual_daa = int(info.get("virtualDaaScore") or 0)
        entries = (_wrpc("getUtxosByAddresses", {"addresses": [address]}) or {}).get("entries") or []
    except Exception:
        return None
    anchor_daa = None
    try:
        d = _wrpc("getDnsConfirmation", {}) or {}
        h = str(d.get("lastDnsConfirmedAnchor") or "")
        if h and set(h) != {"0"}:
            anchor_daa = int(d.get("lastDnsConfirmedAnchorDaaScore") or 0)
    except Exception:
        pass

    maturity, settlement = _coinbase_rule()
    if maturity is None:
        maturity, settlement = _learn_coinbase_rule(address)

    spendable = waiting = rewards = transfers = 0
    waiting_until = []
    for e in entries:
        u = e.get("utxoEntry") or {}
        amount = int(u.get("amount") or 0)
        is_cb = bool(u.get("isCoinbase"))
        block_daa = int(u.get("blockDaaScore") or 0)
        if is_cb:
            rewards += amount
        else:
            transfers += amount
        if maturity is None:
            continue
        if _spendable(is_cb, block_daa, virtual_daa, maturity, settlement, anchor_daa):
            spendable += amount
        else:
            waiting += amount
            # When it clears: the anchor reaching this block, else the fallback.
            waiting_until.append(block_daa + (settlement or 0))

    out = {
        "virtual_daa": virtual_daa,
        "rewards_sompi": rewards,
        "transfers_sompi": transfers,
        "rule": None if maturity is None else {
            "coinbase_maturity_daa": maturity,
            "settlement_daa": settlement,
            "confirmed_anchor_daa": anchor_daa,
            # The fast path the deployed CLI cannot see. False here means every coinbase waits the
            # full fallback, which is a real (slow) state, not a reporting artefact.
            "accelerated": anchor_daa is not None,
        },
    }
    if maturity is not None:
        out["spendable_sompi"] = spendable
        out["waiting_sompi"] = waiting
        out["waiting_until_daa"] = min(waiting_until) if waiting_until else None
    return out


def rewards_sompi(address):
    """(all coinbase paid to the address, the part of it still maturing) — the mining rewards
    the chain has actually handed over, read from the chain rather than from anything this pool
    remembers about what it thinks it paid. An attempt block's reward is escrowed until its claim
    is Final, so it appears here only then. Best effort, like `balance_sompi`: (None, None) when
    the shared node cannot be asked."""
    try:
        r = _wrpc("getUtxosByAddresses", {"addresses": [address]})
        entries = r.get("entries") or []
        total = sum(int((e.get("utxoEntry") or {}).get("amount") or 0)
                    for e in entries if (e.get("utxoEntry") or {}).get("isCoinbase"))
    except Exception:
        return None, None
    immature = None
    try:
        r = subprocess.run(
            [MISAKA, "--network", NETWORK, "--rpc", "127.0.0.1:26313", "--output", "json",
             "wallet", "utxo", "list", "--address", address],
            capture_output=True, text=True, timeout=15,
        )
        if r.returncode == 0:
            # Only coinbase can be immature; a plain transfer is spendable the moment it is in a
            # block. So the wallet's own "immature" bucket is exactly the maturing rewards.
            immature = int((json.loads(r.stdout).get("immature") or {}).get("sompi") or 0)
    except Exception:
        pass
    return total, immature


import datetime as _dt

_PRODUCED_RE = re.compile(r"^(\d{4}-\d\d-\d\d \d\d:\d\d:\d\d\.\d+)([+-]\d\d:\d\d) .*?\[palw-producer\] produced block #\d+ ([0-9a-f]{128})")
_DRAWS_RE = re.compile(r"^(\d{4}-\d\d-\d\d \d\d:\d\d:\d\d\.\d+)([+-]\d\d:\d\d) .*?\[palw-producer\] (\d+) draws this run, (\d+) produced, (\d+) won the class ticket.*?class ticket p = ([0-9.eE+-]+) per draw")


def _log_ts_ms(stamp, tz):
    """`2026-09-05 14:53:29.624` + `+02:00` -> unix milliseconds. The node writes local time with
    its offset; the Studio wants an instant."""
    try:
        t = _dt.datetime.strptime(stamp[:23], "%Y-%m-%d %H:%M:%S.%f")
        sign = 1 if tz[0] == "+" else -1
        off = _dt.timedelta(hours=int(tz[1:3]), minutes=int(tz[4:6])) * sign
        return int((t - off).replace(tzinfo=_dt.timezone.utc).timestamp() * 1000)
    except Exception:
        return None


def _layer0_p(bits):
    """The Layer-0 half of a draw: the share of hashes under the compact target `bits`."""
    try:
        bits = int(bits)
        exp, mant = bits >> 24, bits & 0xFFFFFF
        target = mant * (256 ** (exp - 3)) if exp >= 3 else mant >> (8 * (3 - exp))
        return min(1.0, target / float(2 ** 256))
    except Exception:
        return None


def slot_blocks_and_difficulty(log):
    """(the blocks this slot produced, newest first; the lottery's odds as the node last stated them)."""
    blocks = []
    draws = []
    for l in log:
        m = _PRODUCED_RE.match(l)
        if m:
            blocks.append({"hash": m.group(3), "ts_ms": _log_ts_ms(m.group(1), m.group(2))})
            continue
        m = _DRAWS_RE.match(l)
        if m:
            draws.append((_log_ts_ms(m.group(1), m.group(2)), int(m.group(3)), int(m.group(4)), int(m.group(5)), float(m.group(6))))
    blocks.reverse()
    difficulty = None
    if draws:
        ts, n, produced, ticket_only, p_class = draws[-1]
        rate = None
        # The rate from the last two stamped lines of the same run; a restart resets the counter.
        for prev in reversed(draws[:-1]):
            pts, pn = prev[0], prev[1]
            if pts and ts and pn <= n and ts > pts:
                rate = (n - pn) / ((ts - pts) / 1000.0)
                break
        p_l0 = None
        try:
            info = _wrpc("getBlockDagInfo", {})
            blk = _wrpc("getBlock", {"hash": info["sink"], "includeTransactions": False})
            hdr = (blk.get("block") or blk).get("header") or {}
            p_l0 = _layer0_p(hdr.get("bits"))
            bits = hdr.get("bits")
        except Exception:
            bits = None
        per_block = (1.0 / (p_class * p_l0)) if (p_class and p_l0) else None
        difficulty = {
            "sampled_at_ms": ts,
            "draws_this_run": n,
            "produced_this_run": produced,
            "class_ticket_wins_this_run": produced + ticket_only,
            "class_ticket_p": p_class,
            "layer0_p": p_l0,
            "bits": bits,
            "draws_per_block": per_block,
            "draws_per_s": rate,
            "expected_seconds_per_block": (per_block / rate) if (per_block and rate) else None,
        }
    return blocks[:50], difficulty


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


def address_holdings_sompi(address):
    """Everything the chain holds at an address — mature AND immature — or None when the node did
    not answer. Unlike `balance_sompi` this is a custody question, so an unanswered read is None and
    never zero: a slot whose balance we could not read is a slot we keep."""
    try:
        r = subprocess.run(
            [MISAKA, "--network", NETWORK, "--rpc", "127.0.0.1:26313", "--output", "json",
             "wallet", "utxo", "list", "--address", address],
            capture_output=True, text=True, timeout=15,
        )
        if r.returncode != 0:
            return None
        data = json.loads(r.stdout)
        if not data.get("ok", True):
            return None
        return sum(int((data.get(k) or {}).get("sompi") or 0) for k in ("mature", "immature"))
    except Exception:
        return None


def slot_ids():
    if not os.path.isdir(SLOTS):
        return []
    return sorted(x for x in os.listdir(SLOTS) if re.fullmatch(r"slot-[0-9]{2}", x))


def reclaimable(slot_id, now=None):
    """`(True, why)` when a slot holds nothing anyone could lose by giving its number to a newcomer.

    Every test is one that keeps the slot when in doubt: running, a bond in slot.json, a bond the
    node's own log says it registered (the file is written after the line, so a crash between them
    must not read as "never bonded"), any coins at the address, a balance that could not be read, or
    a creation inside the grace window.
    """
    now = time.time() if now is None else now
    d = os.path.join(SLOTS, slot_id)
    try:
        state = read_state(d)
    except Exception:
        return False, "slot.json is unreadable"
    if unit_active(slot_id):
        return False, "running"
    if state.get("bond_outpoint"):
        return False, "holds a bond"
    if any("registered bond" in l or "already holds" in l for l in tail(os.path.join(d, "kaspad.log"), lines=5_000)):
        return False, "its node logged a bond registration"
    age = now - int(state.get("created_unix") or now)
    if age < SLOT_GRACE_S:
        return False, "created within the last day — its owner may still be funding it"
    held = address_holdings_sompi(state.get("address") or "")
    if held is None:
        return False, "its balance could not be read"
    if held > 0:
        return False, f"holds {held} sompi"
    return True, f"stopped, never bonded, 0 sompi, created {int(age // 3600)} h ago"


def reclaim(slot_id, why):
    """Move a reclaimable slot out of the numbering, keeping every byte of it."""
    d = os.path.join(SLOTS, slot_id)
    state = read_state(d)
    os.makedirs(ARCHIVE, exist_ok=True)
    dest = os.path.join(ARCHIVE, f"{slot_id}-{state.get('created_unix', int(time.time()))}")
    nn = slot_id[5:]
    # Disabled BEFORE the move: an enabled unit on a missing directory restarts every 20 s forever,
    # and on a reboot it would bring the old slot's number back under a stranger's directory.
    for unit in (f"misaka-pool-slot@{nn}", f"misaka-pool-fp@{nn}", f"misaka-pool-fpsubmit@{nn}"):
        subprocess.run(["systemctl", "disable", "--now", unit], capture_output=True, timeout=60)
    os.rename(d, dest)
    with open(os.path.join(dest, "reclaimed.json"), "w") as f:
        json.dump({"reclaimed_unix": int(time.time()), "why": why}, f, indent=1)
    print(f"reclaimed {slot_id} -> {dest}: {why}", flush=True)


def occupancy():
    """`(occupied, reclaimable)` slot ids. Only the cheap test runs for a live slot; the balance is
    asked only of slots that are already stopped, unbonded and old."""
    occupied, free = [], []
    for sid in slot_ids():
        ok, why = reclaimable(sid)
        (free if ok else occupied).append((sid, why))
    return occupied, free


def archived_slot_for(token):
    """What to tell the holder of a reclaimed slot's token: its number may now be someone else's, and
    a bare 403 would read as "your token is wrong" to a person whose token was right."""
    if not token or not os.path.isdir(ARCHIVE):
        return None
    for name in sorted(os.listdir(ARCHIVE)):
        try:
            with open(os.path.join(ARCHIVE, name, "slot.json")) as f:
                state = json.load(f)
        except Exception:
            continue
        if secrets.compare_digest(str(state.get("token") or ""), token):
            why = ""
            try:
                with open(os.path.join(ARCHIVE, name, "reclaimed.json")) as f:
                    why = json.load(f).get("why") or ""
            except Exception:
                pass
            return {
                "error": f"{state.get('slot_id')} was reclaimed ({why}) — join again for a new slot",
                "reclaimed": True,
                "address": state.get("address"),
                "custody": "the reclaimed slot's seed is kept on the pool host, archived, not deleted",
            }
    return None


def mem_available_bytes():
    try:
        with open("/proc/meminfo") as f:
            for line in f:
                if line.startswith("MemAvailable:"):
                    return int(line.split()[1]) * 1024
    except OSError:
        pass
    return None


def capacity_refusal(mode, occupied_count):
    """Why this host takes no new slot of `mode` right now, or None. The two reasons are different
    answers for a joiner — "wait for a slot" and "this host is out of memory" — so they are never one
    sentence."""
    if occupied_count >= MAX_SLOTS:
        return f"the pool is full — all {MAX_SLOTS} slots are running or hold funds"
    avail, need = mem_available_bytes(), MEM_NEEDED_BYTES[mode]
    if avail is not None and avail < need:
        return (f"this host has {avail / 2**30:.1f} GiB of memory available and a {mode} slot needs "
                f"{need / 2**30:.0f} GiB — starting another node here would put the explorer node at "
                f"risk of the OOM killer, so the pool takes no new {mode} slot until memory frees up")
    return None


def create_slot(mode="floor", collateral_override=None):
    os.makedirs(SLOTS, exist_ok=True)
    with open(os.path.join(ROOT, "lock"), "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        occupied, free = occupancy()
        refusal = capacity_refusal(mode, len(occupied))
        if refusal:
            return None, refusal
        for sid, why in free:
            reclaim(sid, why)
        used = slot_ids()
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
            "mode": mode,
        }
        if mode == "fp":
            # Written before the node starts, because the node registers ONE bond and its size is
            # decided in that moment. A slot that starts floor-sized can never become a free-prompt
            # slot; it can only be replaced.
            collateral, why = fp_collateral_sompi()
            if not collateral:
                return None, f"the free-prompt collateral could not be sized: {why}"
            # A caller may size the bond for a LONGER job than the class's canonical one: a
            # 300-token free-prompt claim is ~41 M leaves x slash 5 = ~207 M sompi of exposure,
            # six times the canonical job, and a bond's size is fixed when it registers.
            if collateral_override and int(collateral_override) > collateral:
                why = f"caller-sized: {collateral_override} sompi (the canonical sizing was {collateral}: {why})"
                collateral = int(collateral_override)
            state["bond_collateral"] = str(collateral)
            state["bond_collateral_why"] = why
            state["min_funding_sompi"] = collateral + 200_000_000
            state["fp_class_id"] = FP_CLASS_ID
        with open(os.path.join(d, "slot.json"), "w") as f:
            json.dump(state, f, indent=1)

        subprocess.run(["systemctl", "enable", "--now", f"misaka-pool-slot@{nn}"],
                       capture_output=True, timeout=30)
        return {**state, "seed_hex": seed_hex}, None


# The network's own domain separator, genesis-bound like the class id above. The gateway absorbs it
# into every committed root, and a seat replaying a claim derives it from its node's network name —
# so a wrong value here is a claim nobody can verify rather than an error anyone sees.
FP_NETWORK_DOMAIN = ("d8ef6446d2ebddee9910783ef27c44f726e6f1f925686c937e6b0c10af5543e6"
                     "8d23d4f253501f0175771be517e0ac061b198c2539a7fef69003cbe64cd0c54c")


def fp_port(slot_id):
    return FP_GATEWAY_PORT_BASE + int(slot_id[5:])


def fp_dir(d):
    return os.path.join(d, "fp")


def fp_enable(d, state):
    """**Give this slot the free-prompt lane: an identity from the chain, then two units.**

    The identity is not composed here from what we hope the bond is — `executor_pubkey` and
    `operator_id` are read back from the chain's own view of that bond, so a slot whose bond the
    chain does not know cannot be enabled by accident.
    """
    bond = state.get("bond_outpoint")
    if not bond:
        return None, "this slot has no bond yet — fund it and wait for registration"
    try:
        facts = fp_facts(bond)
    except Exception as e:
        return None, f"the chain could not be asked about this bond: {e}"
    if not facts.get("bondKnown"):
        return None, "the chain does not know this bond"
    if not facts.get("fpCertified"):
        return None, "this chain does not certify the free-prompt lane for this class"
    ceiling = int(facts.get("bondExposureCeiling") or 0)
    per_claim = int(facts.get("bondClaimExposure") or 0)
    if per_claim and ceiling < per_claim:
        return None, (f"this slot's bond cannot carry a free-prompt claim: one costs {per_claim} "
                      f"sompi of exposure and the bond's ceiling is {ceiling}. A bond's size is "
                      f"fixed when it registers, so this needs a new slot created with mode=fp")

    txid, index = bond.split(":")
    identity = {
        "network_domain": FP_NETWORK_DOMAIN,
        "class_id": FP_CLASS_ID,
        "bond_txid": txid,
        "bond_index": int(index),
        "executor_pubkey": facts.get("bondRegisteredPubkey"),
        "operator_id": facts.get("bondOperatorId"),
    }
    if not identity["executor_pubkey"] or not identity["operator_id"]:
        return None, "the chain did not report this bond's pubkey and operator id"

    fp = fp_dir(d)
    os.makedirs(os.path.join(fp, "identity"), exist_ok=True)
    os.makedirs(os.path.join(fp, "outbox"), exist_ok=True)
    with open(os.path.join(fp, "identity", "identity.json"), "w") as f:
        json.dump(identity, f, indent=1)

    nn = state["slot_id"][5:]
    for unit in (f"misaka-pool-fp@{nn}", f"misaka-pool-fpsubmit@{nn}"):
        r = subprocess.run(["systemctl", "enable", "--now", unit], capture_output=True, text=True, timeout=60)
        if r.returncode != 0:
            return None, f"{unit}: {(r.stderr or r.stdout).strip()[:200]}"
    return {"gateway_port": fp_port(state["slot_id"]), "identity": identity}, None


def fp_status(d, state):
    nn = state["slot_id"][5:]
    up = [u for u in (f"misaka-pool-fp@{nn}", f"misaka-pool-fpsubmit@{nn}")
          if subprocess.run(["systemctl", "is-active", u], capture_output=True, text=True,
                            timeout=10).stdout.strip() == "active"]
    bond = state.get("bond_outpoint")
    facts = {}
    try:
        facts = fp_facts(bond) if bond else {}
    except Exception:
        pass
    jobs = 0
    outbox = os.path.join(fp_dir(d), "outbox")
    if os.path.isdir(outbox):
        jobs = len([x for x in os.listdir(outbox) if x.startswith("fp-job-") and x.endswith(".rail.json")])
    return {
        "mode": state.get("mode", "floor"),
        # The market is keyed by LINE, and a class's founding line is the class id — so this is
        # what a caller seeds against or looks the market up by. The name below is prose.
        "line_id": state.get("fp_class_id"),
        "class": FP_CLASS_NAME,
        "gateway_running": f"misaka-pool-fp@{nn}" in up,
        "submitter_running": f"misaka-pool-fpsubmit@{nn}" in up,
        "claims_submitted": jobs,
        "bond_exposure_ceiling": facts.get("bondExposureCeiling"),
        "bond_claim_exposure": facts.get("bondClaimExposure"),
        "fp_certified": facts.get("fpCertified"),
    }


def fp_proxy(handler, state, suffix, body=None):
    """Forward one request to this slot's own gateway.

    The gateway listens on loopback and holds no key; what makes it this slot's is the identity it
    was started with. The token is checked here so the gateway itself never has to learn about
    tokens — it stays the same ordinary HTTP endpoint it is on anyone's laptop.
    """
    import urllib.error
    import urllib.request
    url = f"http://127.0.0.1:{fp_port(state['slot_id'])}{suffix}"
    req = urllib.request.Request(url, data=body, method="POST" if body is not None else "GET")
    if body is not None:
        req.add_header("content-type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=900) as r:
            handler.send_response(r.status)
            ctype = r.headers.get("content-type", "application/json")
            handler.send_header("content-type", ctype)
            handler.end_headers()
            # Relayed in chunks rather than read whole: a free-prompt answer streams for as long as
            # the model takes, and buffering it here would make the app look hung.
            while True:
                chunk = r.read(4096)
                if not chunk:
                    break
                handler.wfile.write(chunk)
                handler.wfile.flush()
    except urllib.error.HTTPError as e:
        handler._json(e.code, {"error": e.read().decode("utf-8", "replace")[:400]})
    except Exception as e:
        handler._json(502, {"error": f"this slot's gateway did not answer: {e}"})


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

    def _slot_and_token(self, slot_id):
        """The slot, if the caller proved they hold it. The token goes in a HEADER — a secret in a
        query string is a secret in every proxy log between here and the app."""
        d = slot_dir(slot_id)
        token = self.headers.get("x-pool-token") or \
            (re.search(r"[?&]token=([0-9a-f]+)", self.path) or [None, ""])[1]
        state = read_state(d) if d else None
        if not state or not secrets.compare_digest(str(token or ""), str(state["token"])):
            gone = archived_slot_for(token)
            if gone:
                self._json(410, gone)
            elif not d:
                self._json(404, {"error": "no such slot"})
            else:
                self._json(403, {"error": "wrong or missing slot token"})
            return None, None
        return d, state

    def do_GET(self):
        if self.path == "/pool/v1/info":
            occupied, free = occupancy()
            fp_collateral, fp_why = fp_collateral_sompi()
            avail = mem_available_bytes()
            return self._json(200, {
                "network": NETWORK,
                "class": "PALW-BASE-0 (the integer floor — no model file, always producible)",
                "slots_total": MAX_SLOTS,
                # Slots that run, hold a bond or hold funds. A stopped, never-funded slot is not
                # counted: the next join reclaims it (archived, never deleted).
                "slots_used": len(occupied),
                "slots_reclaimable": len(free),
                "host_memory_available_bytes": avail,
                "accepting": {m: (capacity_refusal(m, len(occupied)) is None) for m in ("floor", "fp")},
                "refusal": {m: capacity_refusal(m, len(occupied)) for m in ("floor", "fp")},
                "min_funding_sompi": MIN_FUNDING_SOMPI,
                "modes": {
                    "floor": {
                        "class": "PALW-BASE-0",
                        "min_funding_sompi": MIN_FUNDING_SOMPI,
                        "what_it_mines": "the lottery: the job is derived from the block's own position and nobody chooses it",
                    },
                    "fp": {
                        "class": FP_CLASS_NAME,
                        "min_funding_sompi": (fp_collateral + 200_000_000) if fp_collateral else None,
                        "bond_collateral_sompi": fp_collateral,
                        "bond_collateral_why": fp_why,
                        "what_it_mines": "what its owner typed: one execution answers them and commits the claim",
                        "note": "a bond's size is fixed when it registers, so a slot is created for this mode or not at all",
                    },
                },
                "custody": "the slot's producer seed is generated on this host and kept here; "
                           "it is also returned once at join. Rewards accrue at the slot address, "
                           "which that seed controls.",
            })

        path = self.path.split("?")[0]

        m = re.fullmatch(r"/pool/v1/slots/(slot-[0-9]{2})/fp", path)
        if m:
            d, state = self._slot_and_token(m.group(1))
            if not d:
                return
            return self._json(200, fp_status(d, state))

        # The gateway's own surface, as the app already speaks it: `<base>/health` and
        # `<base>/v1/chat/completions`, where `<base>` is this slot's fp URL.
        m = re.fullmatch(r"/pool/v1/slots/(slot-[0-9]{2})/fp/health", path)
        if m:
            d, state = self._slot_and_token(m.group(1))
            if not d:
                return
            return fp_proxy(self, state, "/health")

        m = re.fullmatch(r"/pool/v1/slots/(slot-[0-9]{2})", path)
        if m:
            d, state = self._slot_and_token(m.group(1))
            if not d:
                return
            active = unit_active(state["slot_id"])
            phase, activity, produced = derive_status(d, state, active)
            rewards = rewards_sompi(state["address"])
            funds = funds_breakdown(state["address"])
            blocks, difficulty = slot_blocks_and_difficulty(
                tail(os.path.join(d, "kaspad.log"), lines=20_000, window_bytes=8 * 1024 * 1024))
            return self._json(200, {
                "slot_id": state["slot_id"],
                "address": state["address"],
                "phase": phase,
                "bond_outpoint": state.get("bond_outpoint"),
                "fee_outpoint": state.get("fee_outpoint"),
                "balance_sompi": balance_sompi(state["address"]),
                "rewards_sompi": rewards[0],
                "rewards_immature_sompi": rewards[1],
                "funds": funds,
                "blocks": blocks,
                "difficulty": difficulty,
                "min_funding_sompi": MIN_FUNDING_SOMPI,
                "blocks_won": produced,
                "activity": activity,
            })
        return self._json(404, {"error": "unknown path"})

    def do_POST(self):
        path = self.path.split("?")[0]

        m = re.fullmatch(r"/pool/v1/slots/(slot-[0-9]{2})/fp/enable", path)
        if m:
            d, state = self._slot_and_token(m.group(1))
            if not d:
                return
            result, err = fp_enable(d, state)
            if err:
                return self._json(409, {"error": err})
            return self._json(200, result)

        m = re.fullmatch(r"/pool/v1/slots/(slot-[0-9]{2})/fp/v1/chat/completions", path)
        if m:
            d, state = self._slot_and_token(m.group(1))
            if not d:
                return
            length = int(self.headers.get("content-length") or 0)
            return fp_proxy(self, state, "/v1/chat/completions", self.rfile.read(length))

        if path != "/pool/v1/slots":
            return self._json(404, {"error": "unknown path"})
        mode = "floor"
        collateral_override = None
        try:
            length = int(self.headers.get("content-length") or 0)
            if length:
                body = json.loads(self.rfile.read(length)) or {}
                mode = body.get("mode") or "floor"
                collateral_override = body.get("bond_collateral")
        except Exception:
            pass
        if mode not in ("floor", "fp"):
            return self._json(400, {"error": "mode must be 'floor' or 'fp'"})
        now = time.time()
        if now - _last_join[0] < JOIN_COOLDOWN_S:
            return self._json(429, {"error": "a slot was just created — try again in a minute"})
        slot, err = create_slot(mode, collateral_override)
        if err:
            return self._json(409, {"error": err})
        _last_join[0] = now
        need = int(slot.get("min_funding_sompi") or MIN_FUNDING_SOMPI)
        return self._json(201, {
            **slot,
            "min_funding_sompi": need,
            "next_step": f"send at least {need} sompi, in a normal transfer "
                         f"(not mining rewards), to {slot['address']} — the slot registers its "
                         "bond by itself and starts mining",
            "custody": "this seed also stays on the pool host — that is what running your "
                       "producer for you means. Keep your copy; it controls the rewards.",
        })


if __name__ == "__main__":
    os.makedirs(SLOTS, exist_ok=True)
    ThreadingHTTPServer(("127.0.0.1", 8799), Handler).serve_forever()
