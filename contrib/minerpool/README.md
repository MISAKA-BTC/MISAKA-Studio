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
/etc/systemd/system/misaka-minerpool.service
/etc/systemd/system/misaka-pool-slot@.service
/var/lib/misaka-minerpool/slots/slot-NN/   # seed.key, slot.json, appdir/, kaspad.log
```

Expectations the scripts encode: `/root/t11/kaspad` is the fleet build (the slots must
announce the live fingerprint), `/root/misaka` answers `key gen` and `wallet utxo list`,
the shared node's borsh RPC is at `127.0.0.1:26313`, and slot NN listens on P2P
`17300+NN` / gRPC `27400+NN`. Slots stop with SIGINT — kaspad installs no TERM handler,
and a TERM'd node drops RocksDB on the floor.

## API

```
GET  /pool/v1/info                → capacity, network, minimum funding, the custody sentence
POST /pool/v1/slots               → create a slot: {slot_id, token, address, seed_hex (once)}
GET  /pool/v1/slots/<id>          → X-Pool-Token: phase, balance, bond, blocks_won, activity
```

The phase is derived from the slot node's own log on every read, not from a status field
kept beside it — a status we maintained separately would drift into flattery.

## Two numbers that are not obvious

* **Minimum funding is 10 MSK**, not the chain's 400,000-sompi collateral floor. A UTXO's
  KIP-0009 storage mass grows as the output shrinks, so the smallest *carryable* collateral
  is ~8.34M sompi — the node raises its default to fit and the funding has to cover it.
* **Txids are 128 hex characters** on this chain (PQ hashes). The first version of
  `run-slot.sh` watched for a 64-hex txid, matched nothing, and left a registered bond
  unmined; the regex now accepts any length, and the bond outpoint is written to
  `slot.json` the moment the registration line appears.
