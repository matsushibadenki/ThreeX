# 構造編集の階層検証・隣接表差分化

2026-10-12 JST。構造編集の全Entity検証と全children再構築を廃止した。

検証は直前のgraphが正常という条件で、今回変更されたノードから祖先経路を調べる。
新しい循環は変更ノードを必ず含むため、それらの経路をiterative coloringで検証し、共通祖先を共有する。
親削除は、その親の既存childrenとoverlay／現在の変更で作られた参照を確認する。
削除または付け替えのない子を残す編集は、適用前にMissing parentで拒否する。
グループの途中状態も毎transactionで検証し、後続編集による修復で無効編集を通さない。

隣接表は削除・parent変更の古い辺を先にdetachし、全IDの作成削除後に新しい辺だけattachする。
各childに親children内の位置を持ち、swap_remove後の移動child位置を更新する。
削除slotの再利用前にdetachを完了し、新しいparent IDの解決は全ID更新後に行う。
新規Object・reparent・親削除と子付け替えを同じbatchに含めても扱える。
`UpdateStats::hierarchy_rebuilds`は0、`hierarchy_links`はdetach／attachした辺数を示す。

## 正しさと復旧

新規テスト：子位置の逆引き、swap_remove、孤立拒否の原子性、親／子削除・付け替え・slot再利用、
100回の付け替え、循環拒否、group内の新しい子を持つ親削除の拒否、Undoで階層復元。
追加のjournalテストは親削除＋子の付け替えを1transactionにまとめ、後続recordの末尾切断後も
そのtransactionまで復旧することと、Undoで元の親子関係を復元することを確認した。
既存の全位置切断、record破損、IO失敗・retry、process exit、compaction試験も成功。
core28件＋storage17件＋WGSL1件＋JS4件が成功。core／storageはdebug／releaseで確認。

journal形式・group sync回数・正常prefixまでの復旧・SceneStamp／ID／変換契約は変更なし。
Universal-3DのRust所有・dirty/save境界を再確認したが、相手側ソースは変更せず依存を追加していない。

## 計測方法

macOS arm64、rustc 1.99.0、release thin LTO、offline。4回warmup＋24回測定。
`structure_bench`は初期100／10k Entity、Meshなし、1／8／32編集。
create_eachは全編集で新ID、create_then_updateは1追加後に同じIDのpositionを更新する。
prepare_onlyは準備groupをblack_boxして破棄するCPU時間。journal_syncはprepare/write/sync/apply。
初期構築・保存・コマンド生成は時間外。同期込みではEntity数がサンプルごとに増える。
前後2組。syncは前後とも非空group当たり1回。

最初の組、初期10k／32編集、p95 ms：

| 負荷 | 準備 前 → 後 | 同期込み 前 → 後 |
| --- | ---: | ---: |
| 32件追加 | 9.464334 → 0.008542 | 15.058792 → 7.480291 |
| 1件追加＋31更新 | 0.234541 → 0.004500 | 5.411417 → 4.465667 |

2組目の32件追加は準備8.615084→0.008208 ms、同期込み18.331916→5.275542 ms。
1件追加＋31更新は準備0.257625→0.004292 ms、同期込み5.343666→5.295750 ms。
fsyncは残るため、CPU改善率を同期込みの改善率として扱わない。
小規模では同期の揺らぎによる悪化もあり、100件／8追加は最初の組で4.485833→6.193167 ms。

`hierarchy_bench`は100／10k／100kのMesh Sceneで末端ノードの親を2つのroot間で切り替えるか、
同じ末端IDを削除・再作成する。Scene.applyと借用Instanceのblack_boxを計測し、IO／GPUを含まない。
初期Scene生成とコマンド生成は時間外。こちらは前後1組。

100k、p95 ms：

| 操作 | 前 → 後 |
| --- | ---: |
| 末端reparent | 3.232500 → 0.000584 |
| 末端削除／再作成 | 3.470917 → 0.000625 |

raw CSVに全サイズ、median／p95／p99／maxを保存。24サンプルではp99=maxで尾部精度は限定的。
小さいCPU時間は時計分解能の影響を受ける。一般的な加速倍率、GPU・実表示性能は主張しない。

## 残るコスト

変更数、検証する祖先経路、削除親の子数、影響する子孫に応じた処理は必要。
深い階層や多数の子を持つ親の削除、root Transform等を定数時間とは扱わない。
group overlay参照の検証とhistory管理にも変更ID／履歴数に応じたコストが残る。
逆引きchild_positionsに1slot当たりOption<usize>を追加した。RSSは未測定。
checkpoint保存・読込・圧縮は引き続き全Documentを扱う。各groupのfsyncも維持する。

## 再現

```sh
cargo run --offline --release -p threex-storage --example structure_bench
cargo run --offline --release -p threex-core --example hierarchy_bench
cargo test --offline --workspace
cargo test --offline --release -p threex-core -p threex-storage
cargo clippy --offline --workspace --all-targets --all-features -- -D warnings
```

`../benchmarks/structure-baseline.zip`に変更前core／storageと同じbenchを独立workspaceとして保存。
展開先で同じコマンドを実行できる。rawは`structure-{before,after,repeat-before,repeat-after}.csv`、
`hierarchy-{before,after}.csv`。comparison CSV／metadata JSONも保存。symlink不要。

- 🟢 [Done] 構造検証・辺更新の差分化、正常prefix復旧維持、性能比較。
- 🟠 [Next] worker／Tauri接続・request重複排除、深さ・fanoutを変えた性能試験の拡充。
- ⭕️ [Pending] 実電源断、他OS durability、GPU／実UI検証。

English: Structural validation visits changed ancestor paths and references to deleted parents.
Adjacency updates only affected edges using child-position reverse indices. Journal recovery remains
unchanged. Deep paths, large fanout and affected subtrees still require proportional work.

简体中文：结构验证只检查变更祖先路径及被删除父节点的引用。通过子位置逆向索引，仅更新受影响的边。
日志恢复语义保持不变。深层路径、大量子节点及受影响子树仍有相应处理成本。
