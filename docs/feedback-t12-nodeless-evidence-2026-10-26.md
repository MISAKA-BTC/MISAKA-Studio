# フィードバック:testnet-12 の node なしプロンプトマイニングを成立させるために(2026-10-26)

- 宛先:misakas 開発者・testnet-12 fleet 運用者
- 送り手:MISAKA Studio
- 前回:`docs/feedback-t12-nodeless-mining-2026-10-25.md`(公開 wRPC・executor など)の続き
- 対象:misakas `main` `366763a64`、公開 fleet の rev `247ade1f90b6`、PR [MISAKA-BTC/misakas#124](https://github.com/MISAKA-BTC/misakas/pull/124)

## 0. 要点

node なし(RFC-0009)でプロンプトマイニングをするには、**Panel の seat が miner の material を node 以外から読めること**が必要です。現状の fleet ではこれが成り立っていません。そのため、node なしの miner が claim を出しても、Panel が検証できず、claim は Final になりません。

| # | 問題 | 優先度 |
|---|---|---|
| 1 | 公開 evidence provider が動いていなかった(手順書と `misaka_endpoints` には記載あり) | P0(暫定対処済み、要正式化) |
| 2 | fleet の kaspad は provider を「ディレクトリ」でしか読めず、seat は provider を 1 つも設定していない | **P0** |
| 3 | `palw-a16-fp-worker` が t12 の 8K クラスを処理できなかった | P0(PR #124 で修正、レビュー待ち) |
| 4 | CLI の `host:port` 引数が `wss://` を受け付けず、公開 wRPC だけでは node なしの bond 登録ができない | P1 |
| 5 | fleet が配っている `misaka` CLI に node なしの bond 登録がない、PATH に古い CLI がある | P1 |
| 6 | その他(rail の案内文、gateway のフラグ改名、main の CI) | P2 |

## 1. 公開 evidence provider が動いていなかった

- **事実:** `docs/testnet12-node-free.md` と `misaka_endpoints::testnet12_evidence_provider_v1` は `http://169.58.232.113:8088` を公開 provider としています。しかし 2026-10-25 時点で、169.58.232.113 では `palw-evidence serve` が動いておらず、8088 番も閉じていました。
- **暫定対処(Studio 側で実施済み):** 169.58.232.113 に `misaka-palw-evidence.service` を追加しました。
  - 実行ファイル: `/root/t12-fp/palw-evidence`。PR #124 のブランチ(`8518d78f1`)からビルドしたもので、sha256 は `94f5ad02…`。
  - 起動: `serve --root /var/lib/misaka-palw-evidence --listen 0.0.0.0:8088 --max-store-gb 8`、`MemoryMax=1G`。
  - 外部から到達できることを確認済みです(TLS なし)。
- **依頼:**
  - provider を fleet の正式な構成として管理してください(unit、監視、TLS、保存上限、保持期限と GC)。
  - provider を複数台にしてください(下の 2 と関係します)。`misaka_endpoints` も複数の provider を返す形にしてください。

## 2. fleet の Panel が provider を読めない(本題)

- **事実:**
  - fleet の kaspad(`247ade1f90b6`)の `--palw-evidence-provider-dir` は **ディレクトリのみ**を受け付けます(`palw_panel.rs` の `evidence_provider_dirs: Vec<PathBuf>`)。HTTP の provider を読む変更は `366763a64` で main に入りましたが、fleet にはまだ配布されていません。
  - 8 本の seat(169.58.232.113 の b6・b7、misaka-ibm の b0・b1・b3、5.104.81.23 の b2・b4・b5)は、どれもこのフラグを付けていません。
  - ホスト間に ssh の信頼関係がないので、1 台の provider を他のホストへ同期する手段もありません。
- **結果:** node なしの miner の material は provider にしかなく、Panel は producer からの取得に戻って失敗します。claim は Final になりません。
- **依頼(どちらか):**
  - **A(推奨):** `366763a64` 以降の kaspad を fleet に配布し、各 seat に `--palw-evidence-provider-dir=http://…`(複数)を付けてください。
  - **B(バイナリを変えない暫定案):** 各ホストに provider を置き、seat には自分のホストの provider のルートディレクトリを `--palw-evidence-provider-dir=/var/lib/misaka-palw-evidence` で渡します。node なしの miner(gateway の `--evidence-provider`)は 3 台すべてに publish します(`--evidence-min-copies 2` など)。Studio 側で手順を用意済みで、各 `launch/bN.sh` に 1 行を足して `--check` で検証し、1 本ずつ再起動する形です。**fleet の判断が必要なので、実施していません。**
- **受入条件:** node なしの miner が出した free-prompt claim について、抽選されたどの seat も provider から material を読んで検証し、claim が Final になること。

## 3. `palw-a16-fp-worker` が t12 の 8K クラスを処理できなかった(PR #124 で修正)

- **事実:** worker がモデル ID を `graph-v5@512`(t11 の行)に固定していたため、t12 の genesis の 8K artifact でも `4277d84f…` に解決し、`ebf44d0a…` への要求をすべて拒否していました。
- **修正:** [MISAKA-BTC/misakas#124](https://github.com/MISAKA-BTC/misakas/pull/124)。artifact のヘッダからクラスを導くようにしました(`a16_row_for_artifact_shape_v1`)。同じ PR に次の修正も含めています。
  - main で落ちていた base0 のテスト 4 件。うち 1 件は本物の不具合で、hybrid backend の `attempt_working_set_bytes` が RC 文脈のネットワーク ID で見積もっていたため、8 バイトずれていました。
  - `float_free` ガードで未分類だった 6 ファイルの区分。
- **確認:** misaka-palw-base0 のテストは lib・統合とも全件通過しました。修正版の worker で、公開 wRPC 経由の 8K の回答も確認済みです。
- **依頼:** レビューとマージをお願いします。CI の「Lints」が失敗していますが、main の直近のコミットでも Lints は失敗しています。原因の確認をお願いします。

## 4. CLI の endpoint 引数が `wss://` を受け付けない

- **事実:** `misaka bond register-export --quote-rpc wss://llm.misakascan.com/t12/wrpc/borsh` を実行すると、`ws://wss://…` として接続しようとして失敗します。`--quote-rpc` は `host:port` 前提で、先頭に `ws://` を付けてしまいます。
- **影響:** `register-export` は 2 台以上のノードを要求しますが、公開 wRPC は `wss://` の URL でしか到達できません(`docs/testnet12-node-free.md`)。そのため、node を持たない人は、手順書どおりの node なしの bond 登録ができません。
- **依頼:** `--rpc`、`--quote-rpc`、`--relay` などの endpoint 引数で、`ws://` と `wss://` の完全な URL を受け付けてください(gateway の `chain.rs::normalize_endpoint` と同じ扱い)。

## 5. 配布されている CLI について

- fleet の release に入っている `misaka`(`/root/t12-rel/247ade1f90b6/bin/misaka`、1.1.0)には、`bond register-export / register-sign / register-submit` がありません。登録は kaspad の `--palw-register-bond` 経由だけです。node なしの手順を成り立たせるため、main の CLI を release に含めてください。
- 169.58.232.113 の PATH 上の `misaka`(`/usr/local/bin/misaka`)は古いビルドで、`--network testnet-12` を指定すると `testnet-12 was consolidated into testnet-11` で panic します。整理をお願いします。

## 6. その他

- `misaka-palw-fp-rail --print-identity` は、bond が見つからないとき `register one (kaspad --palw-register-bond …)` と案内します。node なしの手順(`misaka bond register-export / sign / submit`)も案内してください。
- gateway の `--public-job-budget-permille` が `--claim-budget-permille` に改名されていて、旧フラグを使うスクリプトは起動しません。旧名を別名として残すか、release notes に記載してください。
- 169.58.232.113 はディスク使用率が 94%(空き 20 GB)です。

## 7. 運用者が知っておくべき、サーバー上の変更(2026-10-25〜26、Studio 側で実施)

| ホスト | 変更 | 戻し方 |
|---|---|---|
| 169.58.232.113 | `misaka-palw-evidence.service` を追加(上記 1) | `systemctl disable --now misaka-palw-evidence` |
| 169.58.232.113 | pool の fp スロット 04〜06 を t12 の 8K クラス(回答のみ、コミットなし)に切り替え。drop-in `misaka-pool-fp@0{4,5,6}.service.d/t12.conf`、スクリプト `/opt/misaka-minerpool/run-fp-t12.sh`、実行ファイル `/root/t12-fp/` | drop-in を削除して `systemctl daemon-reload` し、再起動 |
| 169.58.232.113 | t11 の rail(`misaka-pool-fpsubmit@04〜06`)を停止。自動起動は有効のまま | `systemctl start` |
| 169.58.232.113 | slot-06 の未署名 bond 登録バンドルを作成(`slots/slot-06/bond-t12/bond-bundle.json`)。**未署名・未送信**で、期限 DAA 15,945 を過ぎているので無効 | 削除して構いません |
| 95.111.236.186 | ビルド用に `/root/misakas-fpfix`(PR #124 のブランチ)を clone して release ビルド。既存の `/root/misakas` には触れていません | ディレクトリを削除 |

kaspad、seat の起動スクリプト、consensus の設定、bond には触れていません。補足として、fp スロットはもともと t11 の設定(t11 のクラス `4277d84f…` と t11 の bond)のまま、t12 ノードのポート `127.0.0.1:26313` を参照しており、正しく動いていませんでした。
