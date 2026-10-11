# Group commitの累積prefix差分化

2026-10-12 JST。グループ内で累積したコマンド列を毎回parse／検証していた処理を廃止。
先行編集の最終属性だけをBTreeMap overlayに保持し、各編集を現在の入力だけでprepareする。
属性のlookup順は「同じ編集内の変更 → 先行編集overlay → live Scene」。
各PreparedTransactionにはその編集の変更分だけを保持し、同期後のapplyも先行変更を再走査しない。
Inverseはその編集直前のoverlay属性から作り、個別revisionとUndo境界を維持する。

## 検証と復旧の契約

作成済み親・子、新ID、削除／再作成、no-opを仮想状態で扱う。非構造編集は先行編集で
検証済みの階層を引き継ぐ。構造編集はlive ID、overlay ID、現在の変更IDの最終状態を使って
親参照・循環・孤立を検証する。最初の無効編集を後続が修復しても、グループは拒否する。
準備だけではlive Scene・履歴を変更せず、PreparedGroupのidentity／revision照合を維持する。

journal形式、record境界、1group=1sync、公開順、失敗時のretry／正常prefix復旧は変更なし。
全Documentの複製や先行コマンド列の再parseはないが、構造変更時の全体階層検証と
apply時の全体隣接表再構築は残る。overlayはグループで変更されたIDだけを保持する。

新規テストは依存する親子作成、Transform更新、循環の事前拒否、削除・再作成、
100グループ×8混在編集の逐次実行との一致を確認。全Undoのsnapshot／revisionも一致する。
作成後の非構造編集でhierarchy_rebuilds=0となることを確認。
既存の末尾全位置切断・record破損・group IO失敗・retry・process exit・compaction障害テストも成功。
core26件、storage16件、WGSL1件、JS4件が成功。core／storageはdebug／releaseで確認。

## 前後計測

macOS arm64、rustc 1.99.0、release thin LTO、offline。初期100／10k Entity、Meshなし。
1／8／32編集、4回warmup＋24回測定、前後2組。シーン・履歴初期構築と保存は時間外。

- create_each：各編集で別の新IDを追加。同期込みではサンプルごとにgroup_sizeだけ増える。
- create_then_update：最初に新IDを追加し、残りはそのIDのpositionを更新。同期込みではサンプルごとに1件増える。
- prepare_only：prepareしたgroupをblack_boxして破棄し、Sceneと履歴は変更しない。IOなし。
- journal_sync：prepare／record write／sync／apply全体。1group当たりsyncは前後とも1回。

コマンド生成は時間外だがslice参照列の生成は時間内。CSV objectsは初期Entity数。
CPU準備と同期込みは別サンプル。GPU／実表示／IPC／電源断性能は測っていない。

最初の前後組、10k／32編集、p95 ms：

| 負荷 | 準備 前 → 後 | 同期込み 前 → 後 |
| --- | ---: | ---: |
| 全件追加 | 20.067125 → 23.289833 | 27.188500 → 27.073542 |
| 1件追加＋31更新 | 17.462875 → 0.722208 | 25.523292 → 5.499250 |

2組目の全件追加は準備18.722917→16.839375 ms、同期込み26.959209→24.530042 ms。
1件追加＋31更新は準備19.271167→0.633625 ms、同期込み23.782834→5.420625 ms。

全件追加は最初の組で準備が約16%悪化した。2組目と方向が異なるため改善を一般化しない。
この負荷は全体構造検証が32回必要で、overlayの追加lookupコストもある。
作成後に非構造更新を続ける負荷では、先行createによる構造検証の繰り返しを除去できた。
残る約5.5 msにはfsyncと一度の構造検証／applyが含まれる。

raw CSVに全サイズとmedian／p95／p99／maxを保存。24サンプルではp99=maxで尾部精度は限定的。
極小準備時間と1編集の差は時計分解能・実行環境の揺らぎの影響を受ける。RSSは未計測。

## 再現と次工程

```sh
cargo run --offline --release -p threex-storage --example prefix_bench
cargo test --offline --workspace
cargo test --offline --release -p threex-core -p threex-storage
cargo clippy --offline --workspace --all-targets --all-features -- -D warnings
```

`../benchmarks/group-prefix-baseline.zip`は変更前core／storageと同じ入力のbenchを保存した独立workspace。
展開後、同じcargo runを実行できる。外部Four／Universal-3Dへの依存なし。
`group-prefix-{before,after,repeat-before,repeat-after}.csv`とcomparison CSV／metadata JSONも保存。

- 🟢 [Done] 累積prefixの再parse／再適用廃止、変更ID overlay、復旧維持、前後2組の性能比較。
- 🟠 [Next] 構造編集の全体階層検証／隣接表再構築の差分化、worker／request重複排除。
- ⭕️ [Pending] 実電源断・実disk-full・他OS durability・Tauri UI接続。

English: Group preparation parses each edit once against a changed-ID overlay and
stores only that edit's delta. Journal format, synchronization and prefix recovery
remain unchanged. Structural edits still scan the full hierarchy; creation-heavy
measurements show mixed results, while create-then-update improves consistently.

简体中文：分组准备通过变更 ID 覆盖层仅解析当前编辑，并只保留其增量。
日志格式、同步及完整前缀恢复保持不变。结构编辑仍扫描整个层级；大量新增的结果有波动，
而新增后连续更新的负载持续改善。
