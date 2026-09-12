# The miner pool — hosted producer slots

What runs at `https://misakascan.com/pool`: a host that already keeps a synced testnet-11
node rents out **producer slots**. A slot is a real `kaspad --palw-produce` with its own
ML-DSA-87 seed, its own bond and its own appdir, supervised by systemd. Joining creates the
slot; **funding the slot's address is the entire remaining ask** — the slot node waits for
the funds itself, registers its bond the way `docs/testnet11-join-mining.md` §3 describes
(sizing collateral against the storage-mass relay floor), captures the printed outpoint,
and restarts as a producer with the carrier's change as its fee outpoint.

This is **not** a work-splitting pool, because the protocol does not permit one: the thing
that runs the model is the thing that makes the block, and it must hold the bonded key. So
"mine without a node" is precisely "someone else runs your producer", and this service says
that instead of hiding it — the slot seed is generated on the pool host, **stays there**,
and is returned exactly once in the join response. The Studio writes that copy to a 0600
file so the rewards are recoverable without the pool's cooperation. On a test network that
trade is the product; on mainnet it would be a custody business, which is why this lives in
`contrib/` and not in the node.

## Deployment (one host)

```
/opt/misaka-minerpool/pool.py        # the API, 127.0.0.1:8799, nginx-proxied at /pool/
/opt/misaka-minerpool/run-slot.sh    # slot lifecycle: register → capture outpoint → produce
/opt/misaka-minerpool/run-fp.sh      # a free-prompt slot's gateway (holds no key)
/opt/misaka-minerpool/wrpc.py        # the explorer node's JSON wRPC, for pool.py's chain reads
/etc/systemd/system/misaka-minerpool.service
/etc/systemd/system/misaka-pool-slot@.service
/etc/systemd/system/misaka-pool-fp@.service
/etc/systemd/system/misaka-pool-fpsubmit@.service
/etc/systemd/system/misaka-pool-fpsubmit@.service.d/watch.conf   # runs misaka-palw-fp-rail --watch
/var/lib/misaka-minerpool/slots/slot-NN/   # seed.key, slot.json, appdir/, kaspad.log, fp/
/var/lib/misaka-minerpool/slots/archived/  # reclaimed slots, kept whole (their seeds are custody)
```

The files here are the ones deployed on the pool host (synced 2026-09-12; the copy had fallen
behind by the whole free-prompt mode). `misaka-pool-fpsubmit@.service` still names the retired
`fp-autosubmit.py`; the drop-in replaces its `ExecStart`, and is what runs.

## Capacity: what holds a slot, and what the host can carry

A slot number is held while the slot **runs, holds a bond, holds any coins, or is less than a day
old**. Anything else — stopped, never bonded, 0 sompi, older than a day — is reclaimed by the next
join: its units are disabled and its directory is moved to `slots/archived/`, never deleted. Every
test keeps the slot when in doubt (an unreadable balance, a bond the node logged but slot.json does
not yet name). A reclaimed slot's token answers `410` with what happened, not a bare `403`.

Before this, the pool counted directories: two slots stopped on 2026-09-05 during the host's OOM
incident (never funded, units dead) made every join answer "the pool is full" for a week.

The slot count is not the only limit. A join is refused while the host's `MemAvailable` is under
3 GiB (floor) or 8 GiB (free-prompt), because the explorer node shares this host and was OOM-killed
there on 2026-09-05. `/pool/v1/info` reports `slots_used` (running or holding something),
`slots_reclaimable`, `host_memory_available_bytes`, and per mode `accepting` and `refusal`.

Expectations the scripts encode: `/root/t11/kaspad` is the fleet build (the slots must
announce the live fingerprint), `/root/misaka` answers `key gen` and `wallet utxo list`,
the shared node's borsh RPC is at `127.0.0.1:26313`, and slot NN listens on P2P
`17300+NN` / gRPC `27400+NN`. Slots stop with SIGINT — kaspad installs no TERM handler,
and a TERM'd node drops RocksDB on the floor.

## API

```
GET  /pool/v1/info                → capacity (and why a join would be refused), minimum funding, custody
POST /pool/v1/slots               → create a slot: {slot_id, token, address, seed_hex (once)}
GET  /pool/v1/slots/<id>          → X-Pool-Token: phase, balance, bond, blocks_won, activity
```

The phase is derived from the slot node's own log on every read, not from a status field
kept beside it — a status we maintained separately would drift into flattery.

## Two numbers that are not obvious

* **Minimum funding is 10 MSK**, not the chain's 400,000-sompi collateral floor — and the
  misakascan faucet's grant is set to 12 MSK *because* of this: a faucet that hands out less
  than one bond's worth would make "get funds from the faucet" a lie. One grant per address,
  one per source per day (`misaka-faucet.service` on the fleet, `FAUCET_GRANT=12`). A UTXO's
  KIP-0009 storage mass grows as the output shrinks, so the smallest *carryable* collateral
  is ~8.34M sompi — the node raises its default to fit and the funding has to cover it.
* **Txids are 128 hex characters** on this chain (PQ hashes). The first version of
  `run-slot.sh` watched for a 64-hex txid, matched nothing, and left a registered bond
  unmined; the regex now accepts any length, and the bond outpoint is written to
  `slot.json` the moment the registration line appears.
