# SoA・ワールドTransform差分更新の性能比較

この工程後、Rendererの全Instance比較・コピーは差分履歴に置き換えた。
当時の制約と計測は下記に保存し、現在の送信準備の結果は`upload-performance.md`を参照。

2026-10-11、同一の開発ホスト（macOS / arm64、Rust 1.99.0）で実施。

Problem: 1件の変更でも描画データ取得時に全Objectのワールド行列を再計算し、
毎回BTreeMapキャッシュとInstance Vecを生成していた。深い階層は再帰で評価していた。

Root cause: ObjectのAoSマップを描画のたびに走査する構造。永続ワールド行列と
子孫への変更伝播、描画Instanceの保持がなかった。

Evidence: 改修前の実装を保存し、同じバイナリ入力・同じベンチマークソースで比較。
`benchmarks/soa-before.csv`と`soa-after.csv`が生データ、`soa-comparison.csv`が差分。
`soa-metadata.json`にソースハッシュ・計測境界を記録した。

Changed files:

- `crates/core/src/transform_store.rs`: position / rotation / scale / color / parent / visibility等のSoA配列。
- `crates/core/src/scene.rs`: 検証・保持キャッシュ・差分伝播・反復評価。
- `crates/core/src/lib.rs`: 型、変換数学、Universal-3D変換、公開API。
- `crates/renderer/src/lib.rs`: Instanceスライスの借用と既存比較用Vecの容量再利用。
- `crates/core/examples/transform_bench.rs`, `transform_cost_bench.rs`: 再現用計測。

Change: Entity IDから内部スロットへの索引のみBTreeMapに残し、属性は配列に分離。
削除スロットを再利用するが外部には公開しない。完全なバッチ検証後に変更を反映し、
ローカル行列はTransform変更時だけ、ワールド行列は変更対象と子孫だけ評価する。
親が先になる反復処理で重複更新を防ぐ。可視性・色の変更は行列計算を無効化しない。
保持Instanceは通常変更で直接書き換え、読み取りは借用スライスを返す。
構造変更の検証は反復方式にし、ルートだけの要素には訪問状態を作らない。
新規追加件数に応じてSoA配列容量を事前確保する。

Why it should improve performance: 全体の描画用再構築を変更部分の更新に置き換える。
未変更の行列とInstanceを再利用し、静止シーンでの再計算・割り当てをなくす。
改善の主因は保持キャッシュと差分処理であり、SoA単独の速度向上と断定しない。

## 計測条件

- Release + thin LTO、同一ホスト、入力は100 / 1,000 / 10,000 / 100,000件。
- `apply(binary commands) + instances()`のCPU描画準備を計測。
- 通常ケースは8回ウォームアップ後80回、構築・全件更新は4回後20回。
- コマンド生成は計測外。インスタンス取得時の旧実装の割り当て・破棄は計測内。
- 通常ケースは1親＋10子、その他は独立したObject。可視性ケースは1件を交互に表示／非表示。
- 全件更新は全Objectの位置を交互に変更。初期構築は空Sceneへの生成＋初回描画準備。
- quantileは`ceil((N-1)*p)`。この標本数ではp99=max、20回ケースはp95もmaxになる。
- 微小時間はタイマー分解能に近い。厳密な倍率やGPUフレーム時間として解釈しない。

## Before / After

10万件のp95、単位ms。全サイズのmedian / p95 / p99 / maxはCSVを参照。

| 操作 | Before | After | 差分 |
| --- | ---: | ---: | ---: |
| 静止状態の描画準備 | 12.227917 | 0.000042 | -12.227875 |
| 葉1件のTransform変更＋描画準備 | 11.922875 | 0.000209 | -11.922666 |
| 親1件＋10子の更新＋描画準備 | 12.779375 | 0.000417 | -12.778958 |
| 色1件の変更＋描画準備 | 11.967291 | 0.000167 | -11.967124 |
| 1件の表示切り替え＋描画準備 | 12.007959 | 0.441833 | -11.566126 |
| 初期構築＋初回描画準備 | 32.479375 | 31.606250 | -0.873125 |
| 全件Transform変更＋描画準備 | 25.753792 | 21.488792 | -4.265000 |

小規模ケースでは構築・全件更新の追加管理コストがある。
100件の初期構築medianは0.010125→0.039708ms、全件更新は0.007500→0.022334ms。
1,000件の初期構築medianは0.147834→0.379375ms、全件更新は0.103333→0.186125ms。
これを隠して全ケースで高速化したとはしない。10万件の初期構築はほぼ同程度で、
通常の静止・少数編集と全件更新では改善した。小規模な初期処理のコスト増を許容し、
大規模な継続フレーム処理の削減を採用理由とする。

## 処理量の回帰検証

`update_stats()`のカウンターと保持スライスのアドレスをテストで確認。

| 10万件のSceneに対する操作 | Local計算 | World計算 | Instance書換 | 全体packing再構築 |
| --- | ---: | ---: | ---: | ---: |
| 葉1件のTransform変更 | 1 | 1 | 1 | 0 |
| 親1件（10子）のTransform変更 | 1 | 11 | 11 | 0 |
| 色1件の変更 | 0 | 0 | 1 | 0 |
| 同じ値の更新／空バッチ | 0 | 0 | 0 | 0 |

通常更新はInstanceの割り当てをやり直さず、無関係のObjectを評価しない。
表示変更・Mesh化・追加削除で描画対象集合が変わるとpackingを再構築する。

CPU impact: 上記実測。深さ2万の階層も再帰なしで生成・更新できる。
構造変更の階層検証・隣接リスト再構築は全体処理として残る。

GPU impact: wgpuの描画形式は同じ。今回の計測はGPUを含まない。
rendererは依然として全Instanceの比較・比較用コピーを行うため、
このCPU準備時間をそのままフレーム時間改善として扱わない。

Memory impact: Local/World各64B、描画Instance80Bなどを保持するため、
永続キャッシュの容量は増える。一方、毎回の全体キャッシュ・Vec生成を廃止した。
`resource.getrusage(RUSAGE_CHILDREN)`で同じ構築／全件更新ベンチマークの別プロセスを計測し、
ピークRSSは450,265,088→289,275,904 bytesだった。
これは全サイズ・繰り返し構築・入力バッファ・アロケーターを含むプロセス全体の最大値であり、
単一Sceneの常駐容量でも本番メモリ削減率でもない。生データは`soa-memory-*.json`。
削除スロット・配列・子リスト・packed Vecの容量は再利用のため保持し、即時縮小しない。
`time -l`による計測はsandboxのsysctl制限で失敗したため、JSONのgetrusage計測を使用した。

I/O impact: 実行ループに追加のI/OやIPCはない。
Energy impact: 電力・GPU使用率は未測定。省電力効果は断定しない。

Correctness verification: Debug/ReleaseのRust Sceneテスト12件、WGSL/upload-rangeテスト2件、
JavaScript契約テスト4件、Clippyとformatチェックが成功。
600回のTransform／色／表示／Mesh化／再親子化混合更新後のワールド行列・描画配列を
独立した全体再計算の参照と比較。親子同時変更、削除・再親子化、スロット再利用、
不正バッチの原子性、深い階層も検証した。
Universal-3Dの度・逐次X/Y/Z変換・ワールドミラーを新しいSoA経路でも確認。

Regression risk: Rust `instances()`は所有Vecから借用スライスに変わった。
独立所有が必要な利用者は`.to_vec()`する。JS API／バイナリ形式は変えていない。
構造・可視性の変更はO(N)処理を含む。GPUの実描画・WASMビルド検証は引き続きPending。
Resource generation handlesは別工程であり、内部スロットの再利用とは区別する。

Keep / Revert: Keep。大規模な差分更新で明確なCPU処理量削減を確認したため採用。
小規模構築のコスト増と永続キャッシュのメモリコストを明示し、全体比較・packingを次の改善対象とする。

## 再現

```sh
cargo run --offline --release -p threex-core --example transform_bench
cargo run --offline --release -p threex-core --example transform_cost_bench
python3 docs/benchmarks/measure_rss.py target/release/examples/transform_cost_bench
```

旧実装は`benchmarks/soa-baseline-core.rs.txt`に保存。独立した一時crateの`src/lib.rs`として配置し、
package名を`threex-core`、editionを2024、dependencyを`bytemuck 1.25`（derive）に設定する。
profile.releaseの`lto = "thin"`を指定し、同じexampleファイルで実行すると比較できる。
このsnapshotは記録用で、現在のアプリ／crateからは参照しない。
Universal-3Dのシンボリックリンクもコード・テスト・ビルド依存に含めていない。
