# フィードバック:testnet-12 で node なしマイニング(RFC-0009 stage A)を実際に使える状態にするための不足点

- 宛先:misakas メンテナ(RFC-0009 / PALW remote / fleet 運用)
- 送り手:MISAKA Studio
- 日付:2026-10-25
- 確認対象:misakas `main` `d2214e057`(公開 fleet が動かしている rev `247ade1f9` の公開版)、RFC-0009 実装記録、fleet への外部からの接続確認

## 1. 要望

MISAKA Studio から、**ユーザーが `kaspad` を立てずに testnet-12 で PALW マイニング**(RFC-0009 stage A の ordinary attempt)を始められるようにしたい。Studio 側は `palw-remote-miner` を子プロセスとして起動・監視する形で組み込む予定。

現状、`palw-remote-miner` 自体はビルドでき、設計上 consensus fence も不要(node が block を独立に検証する)だが、**公開 testnet-12 に対しては次の理由で動かせない**。以下を、優先度の高い順に依頼する。

## 2. 確認した事実

| 項目 | 確認結果 | 根拠 |
|---|---|---|
| fleet の外部公開ポート | `169.58.232.113` と `169.58.39.220` は **P2P `26311` のみ**応答。wRPC Borsh `27210`、wRPC JSON `28210`、gRPC `26210` は外から接続できない(`169.58.232.114` は全ポート応答なし) | 2026-10-25 に `nc -z` で確認 |
| remote miner の RPC 要件 | `--rpc` に **独立した node が 2 台以上**必要(`die("--rpc needs at least two independent nodes…")`、`QuorumPolicy.min_agree` は 2 未満に下がらない) | `misaka/palw/remote/src/bin/palw-remote-miner.rs`、`view.rs` |
| 公開 RPC endpoint の一覧 | `misaka/endpoints` はローカル registry(`~/.misaka/<net>/endpoints.json`)のみで、testnet-12 の公開 endpoint を持たない | `misaka/endpoints/src/lib.rs` |
| executor | **バイナリに含まれない**。`--executor-cmd` が受け口で、モデル実行は `kaspad/src/palw_backends` から未抽出(CODE_GAP) | 同バイナリ冒頭の doc、RFC-0009 実装記録 §3 |
| 実ノードでの実行 | 「**No case below has been run against a live node or network**」、バイナリは「never run against a node」 | RFC-0009 実装記録 冒頭・§3 |
| material の配信 | miner は `--state-dir/material/<attempt id>` に保存し、**PC を起動したまま配信**する必要がある。kaspad の `--palw-evidence-provider-dir` は **ディレクトリのみ**で URL provider を受けない(CODE_GAP small)。provider discovery は DESIGN_GAP | 実装記録 §3・§5 |
| L2(fork choice の検証) | `palw_fork_choice_commitment_v1` は全ネットワークで dormant。L2 は未検証のままで、`--accept-unverified-state` が要る | バイナリ冒頭の doc |
| V4 公開 redemption | `palw_receipt_spend_v4` は全 preset で `None`(stage C は dormant) | `consensus/core/src/config/params.rs` |
| node なし bond 登録・モデル登録 | `misaka bond register-export/sign/submit`、`misaka model add --export-bundle / sign / submit` は実装済み(unit/file テスト)。ただし submit 先の relay も RPC が必要 | 実装記録 §1、Round 2 |

## 3. 不足点と依頼内容

### P0-1. testnet-12 の公開 wRPC endpoint(最低 2 台、独立運用)

**不足:** fleet の RPC が外部から到達できず、remote miner の quorum(2 台以上)を満たせない。node なし bond 登録・モデル登録の `submit --relay` も同じ理由で使えない。

**依頼:**
- 公開用の wRPC(Borsh、できれば JSON も)を **2 台以上、別ホスト・別運用主体**で提供する。TLS 終端(`wss://`)を推奨。
- remote miner が呼ぶ RPC を公開対象に含める:`getBlockDagInfo`、`getBlockTemplate`、`getPalwProducerFacts`、`submitBlock`、`getPalwStateProof`(op 202)、`getPalwForkChoiceOpening`(op 203)、登録・carrier の submit と追跡に使う RPC。
- 書き込み系(`submitBlock`、transaction submit)の rate limit と、管理系 RPC を公開しないための allowlist 方針を決める。
- endpoint を `misaka/endpoints` かリリース資料に testnet-12 の既定値として載せ、Studio と CLI が同じ一覧を読めるようにする。

**受入条件:** 外部ホストから 2 台に `getBlockDagInfo` と `getPalwStateProof` が通り、2 台の `virtual_daa` の差が `max_daa_skew` 以内に収まる。fleet の node が op 202/203 に対応している(未対応の node は WebSocket を切ると記録にある)ことを確認する。

### P0-2. executor(モデル実行)を `palw-remote-miner` から使える形で提供

**不足:** `--executor-cmd` に渡す実行プログラムが存在しない。Panel が再実行できない roots を出せば不正になるため、Studio 側で独自の executor を書くことはできない(consensus と同じ実装である必要がある)。

**依頼:**
- `kaspad/src/palw_backends` の attempt 実行部を抽出し、`PALW_ANCHOR` を受けて `trace_root` / `output_root` / `execution_root` / `trace_manifest_root` / `trace_chunk_count` / `material_file` を JSON で返す **公式 executor バイナリ**(例:`palw-attempt-exec`)を提供する。
- 少なくとも floor(`PALW-BASE-0`、`f1c5635c…`)と 8K 行(`ebf44d0a…`、artifact `qwen25-1.5b-a16-8k.palwart`、inventory root `88096dc1…`)に対応する。
- kaspad 内蔵の producer と **同じ anchor で同じ roots と material を出すこと**を確認するテストを付ける。

**受入条件:** 同一の anchor・class・artifact で、`kaspad --palw-produce` 経路と executor の出力 roots がビット一致する。

### P0-3. 公開 testnet-12 での end-to-end 実行記録

**不足:** remote miner は実ノード・実ネットワークで一度も動いていない。

**依頼:** P0-1・P0-2 の後に、公開 testnet-12 で「template 取得 → 実行 → 当選時のみ署名 → 全 node へ submit → block 追跡 → claim が Panel に検証されて `Final`」までを 1 件通し、実装記録に残す(Panel 側が miner の material を取得できたかを含む)。

### P1-1. material の配信先(PC を常時起動しなくてよくする)

**不足:** 現状は miner の PC が material を配信し続ける必要がある。fleet Panel の `--palw-evidence-provider-dir` はディレクトリのみで、HTTP provider から読めない。

**依頼:**
- `palw_panel.rs` で `ProviderSpecV1`(URL)を受け付ける小変更(記録上 5 行程度)を入れ、fleet Panel が HTTP provider から material を読めるようにする。
- testnet-12 用の公開 evidence provider(`palw-evidence` の reference server)を最低 1 つ用意し、remote miner の `palw-evidence publish` 先として既定に載せる。
- provider discovery の暫定方針(リリース資料に固定リストを載せる等)を決める。

**受入条件:** remote miner が provider に publish した material を、fleet Panel が URL 経由で取得して検証できる。

### P1-2. 信頼起点(checkpoint)の配布

**不足:** `--checkpoint <daa>:<hash>`(と任意の `--pin`)は利用者が用意する必要があるが、署名付き checkpoint の公式配布がない。

**依頼:** testnet-12 の署名付き checkpoint を定期的に公開する(リリース資料、または `palw-fork-choice-issuer` の公開鍵と attestation 置き場)。Studio はこれを既定の信頼起点として表示・検証する。

### P1-3. 手数料 float と lifecycle carrier の node なし運用手順

**不足:** testnet-12 では carrier(登録・receipt・readiness・court 応答)に fee float が要るが、node なしでの提出経路は relay の submit に依存し、手順がまとまっていない。

**依頼:** node なしで bond 登録 → declare → 採掘開始 → 報酬確認までの手順を、testnet-12 の現行規則(producer floor 13,000 BILI、claim ごとの担保)で 1 本の資料にする。faucet が未整備(TBD)である点の扱いも明記する。

### P2. 将来の fence(参考)

- `palw_fork_choice_commitment_v1`(L2 の検証)と `palw_receipt_spend_v4`(V4 公開 redemption、free-prompt の node なし回収)は全ネットワークで dormant。node なしで「検証済み」と表示できる範囲と、free-prompt を node なしで回収できる時期を、activation の予定とともに示してほしい。

## 4. MISAKA Studio 側で行うこと(上記と並行)

- `palw-remote-miner` を子プロセスとして起動・監視する「node なし」モードを追加する(RPC 2 台以上、bond、class、artifact root、受取アドレス、鍵ファイル、checkpoint、state dir、executor を設定)。
- 起動前に、P0 の前提(公開 RPC 2 台の合意、executor の有無、op 202 対応)を検査し、欠けていれば理由を表示して開始しない。
- `UNVERIFIED_REMOTE_STATE` と `VERIFIED_REMOTE` を区別して表示し、material を配信中の間は PC を止めないよう案内する。
- testnet-12 への切り替え(既定ネットワーク、`--netsuffix=12`、ジェネシスクラス表 floor・8K・2M)は Studio 側で対応済み(未コミット)。

## 5. 対応状況(misakas `main` `366763a64`、2026-10-25 確認)

| 項目 | 状況 | 確認 |
|---|---|---|
| P0-1 公開 wRPC 2 台 | **対応済み**。`wss://wallet.misakascan.com/t12/wrpc/{borsh,json}` と `wss://llm.misakascan.com/t12/wrpc/{borsh,json}`、`misaka_endpoints::testnet12_public_wrpc_v1`、`--rpc` 省略時の既定値 | 外部から両方の JSON に `getBlockDagInfo` が通り、同じ DAA 15,538 と sink `b59479ad…` を返した。2 台とも同じ運用主体(misakascan)である点は残る |
| P0-2 公式 executor | **対応済み**。`palw-attempt-exec`(floor と 8K)、producer と同じ経路で floor の一致テストあり | コードとテスト名のみ確認、実行はしていない |
| P0-3 公開ネットでの e2e 記録 | **未確認**。手順書(`docs/testnet12-node-free.md`)はあるが、実行記録は見当たらない | — |
| P1-1 material 配信 | **一部対応**。fleet panel が `http(s)://` provider を受けるようになった。公開 provider `http://169.58.232.113:8088` は外部から接続できなかった(ポートが閉じている) | `nc -z`、`curl` で確認 |
| P1-2 checkpoint 配布 | **未対応**。手動ピン(`getBlockDagInfo` の値を自分で確認して渡す)のまま | 手順書に明記 |
| P1-3 node なし手順書 | **対応済み**。`docs/testnet12-node-free.md` | — |
| P2 fence | `palw_receipt_spend_v4` は testnet-12 で DAA 15,500 に有効(現在 15,538 なので有効済み)。`palw_fork_choice_commitment_v1` は dormant のまま | 手順書 |

## 6. 追加:node なしプロンプトマイニングの実測で見つかった不足(2026-10-25、misakas main)

MISAKA Studio から `misaka-palw-gateway` と `misaka-palw-fp-rail --watch` を公開 wRPC 向けに起動して確認した。gateway は `wss://wallet.misakascan.com/t12/wrpc/borsh` で起動でき、`registered true | fp_certified true` を読めた。

### P0-4. `palw-a16-fp-worker` が testnet-12 の 8K クラスを処理できない

- **事実:** worker は `MODEL_ID = misaka_palw_base0::classes::A16_GRAPH_V5_MODEL_ID` に固定されている。testnet-12 の 8K artifact(`qwen25-1.5b-a16-8k.palwart`)を渡しても `Qwen/Qwen2.5-1.5B/graph-v5@512`(クラス `4277d84f…`、n_ctx 512、testnet-11 の行)として解決する。
- **結果:** 8K クラス `ebf44d0a…` で投げた要求は `class_id mismatch — the request declares a runtime this worker is not (ours 4277d84f…, request ebf44d0a…)` で拒否される。testnet-12 の genesis には 512 行がないため、**t12 で node なしプロンプトマイニングを行う worker が存在しない**。
- **依頼:** worker が artifact のヘッダからクラスを導く(コード内のコメントにある `classes::a16_artifact_row_v1` の利用)か、`A16_GRAPH_V7_8K_MODEL_ID`(`Qwen/Qwen2.5-1.5B/graph-v7@8192`)を選べるようにする。
- **受入条件:** 8K artifact で worker が `ebf44d0a…` に解決し、gateway 経由のチャット要求に回答し、commitment を outbox に出す。

### P1-4. node なし identity の取得手順

`misaka-palw-fp-rail --print-identity` は bond が見つからないと `register one (kaspad --palw-register-bond …)` と案内する。node なしの登録手順(`misaka bond register-export / sign / submit`)を案内する文言にしてほしい。
