> このレポートは差分送信導入時の記録です。現在はmembership変更も差分同期します。最新の仕様・計測は `packing-performance.md` を参照。

# GPU送信前のInstance比較・コピーを差分同期へ置き換え

2026-10-11。計測はmacOS arm64 / Rust 1.99.0のCPU送信準備で、GPU実行は含まない。

Problem: Sceneが1件更新されるだけでもRendererが全Instanceをバイト比較し、
全Instanceを比較用Vecへコピーしていた。10万件では1描画あたり8MBのCPUコピーだった。
revisionだけの比較では同じrevisionを持つ別Sceneを区別できなかった。

Root cause: Rendererが変更箇所を自力で検出するために全体スナップショットを保持していた。

Evidence: 旧Scene実装と旧送信準備の処理を独立したcrateとして保存し、同じ入力を使って
新実装と比較した。初回の全件更新の裾が悪化したため、before→afterの対をさらに3回計測。
生データは`benchmarks/upload-before.csv`, `upload-after.csv`, `upload-repeat-*.csv`。
初回差分は`upload-comparison.csv`、3回の集計は`upload-repeat-summary.json`。

Changed files: `crates/core/src/instance_updates.rs`, `scene.rs`, `lib.rs`,
`crates/renderer/src/lib.rs`, `crates/core/examples/upload_bench.rs`。

Change: Instanceを書き換える時点で連続範囲を集め、Sceneにrevision付き履歴として保持。
Rendererは変更範囲のスライスを借りてqueueへ登録し、新しいSceneStampを保持する。
全Instanceの比較関数と比較用Vecは削除。範囲履歴にInstance本体はコピーしない。

Why it should improve performance: GPU送信前の検出・コピーをO(N)から変更範囲の
メタデータ処理に置き換える。GPUに送るInstanceの本体はSceneから直接借用する。
複数描画先のカーソルは独立し、同じ履歴を互いに消費せず読める。

## 差分履歴の契約

- Sceneの同一性をArcの識別子で保持し、同じrevisionの別Sceneも全量同期する。
- 最大64 revision、合計4096範囲のメタデータを保持する。
- 描画間の複数更新は範囲を統合し、重複・隣接をまとめる。
- 初回・Scene切り替え・GPUバッファ再確保・履歴切れ・packing変更は全量同期する。
- 1 revisionの書き込み順のrun数が1024を超えた場合も全量マーカーに戻す。
- 空Sceneは転送範囲なし、描画Instance数0として同期する。
- 空／同値バッチは履歴を進めず、失敗したバッチは履歴も変えない。
- 書き込みの登録が終わる前にカーソルを更新しない。失敗時は古いstampから再取得できる。
- 範囲は最新の`Scene::instances()`の借用スライスに対して適用する。

更新対象が少ない通常ケースで全体比較・CPUコピーを行わない。
古い履歴を失った描画先に対して、完全な最新状態を優先して全量転送する。

## 計測条件

Release + thin LTO。100 / 1k / 10k / 100k Instance、12回ウォームアップ後128標本。
median / p95 / p99 / maxを記録。quantileは`ceil((N-1)*p)`。
旧実装は全バイト比較＋比較用Vecの容量再利用コピー、新実装はstamp・履歴から範囲を取得。
両方にSceneの変更適用を含む。初期構築と初期同期は計測外。
1件更新、8回の変更をまとめて同期、全件Transform更新を用意。
全件コマンド生成は計測外、1件／8件ケースの固定長コマンド作成は両方とも計測内。
結果をblack_boxに渡し、新実装のInstanceスライスも公開して計算が除去されないようにした。
GPU queue内部のstagingコピー・GPU転送・描画・JS/WASM境界は測定していない。

## Before / After

10万件。追加の3回のbefore/after対について、各回のp95の中央値（ms）。
標本をプールしたp95ではない。全サイズ・初回結果・各回の値はCSV/JSONを参照。

| CPU送信準備（変更適用を含む） | Before | After |
| --- | ---: | ---: |
| 1件のTransform更新 | 0.432667 | 0.000292 |
| 8回の更新をまとめて同期 | 0.434791 | 0.001709 |
| 全件Transform更新 | 22.208125 | 22.283292 |

1件・8件の更新は明確に改善。微小な新実装の値はタイマー分解能に近いため、
厳密な高速化倍率を主張しない。全件更新では差分にする利点が小さく、改善とは判断しない。
全件更新のp95は各回before=21.995 / 22.208 / 22.504ms、
after=22.283 / 29.153 / 21.980msと変動し、2回目の裾は悪化した。
各回medianはbefore=21.739 / 21.927 / 21.926ms、
after=21.336 / 21.525 / 21.376ms。中央値と裾を区別して記録する。
実GPU環境でのフレーム時間と全件更新の裾の再評価はPending。

CPU impact: 全Instanceのバイト比較・比較用コピーを廃止。
履歴の参照・範囲統合と記録のコストを追加。全件のWorld更新自体は必要であり残る。

GPU impact: 1件の通常更新は1 Instance、80 bytesの範囲になる。
旧方式もその範囲だけをGPUへ送っていたため、通常時のGPU転送量が減ったという主張はしない。
全量fallbackや保守的なdirty通知では旧バイト比較より送信量が増える場合がある。
GPUタイミング・queue登録時間は未測定。

Memory impact: Rendererの比較用8MB（10万件）と、更新時の同量のCPUコピーを廃止。
これは型とコード上のpayload量であり、プロセスRSSの測定値ではない。
代わりにSceneに最大4096範囲＝64KiBの有効メタデータ（64bit Range）を保持。
Vecの予約容量、Record、Arc、クエリーの一時範囲Vec等の追加コストは別。
Sceneの保持InstanceとGPUバッファは引き続き必要であり、zero-copy GPU転送は主張しない。

I/O impact: ディスク・ネットワーク・IPCを追加しない。
Energy impact: 電力未測定。省電力の具体的数値は主張しない。

Correctness verification: Rust 16件、WGSL検証1件、JS契約4件、Clippyとformatチェックが成功。
10万件の1件更新・複数更新統合・独立した2描画先のカーソルを検証。
可視性によるpacking変更、空Scene、追加削除、強制全量再同期、履歴切れ、範囲数上限、
同revisionの別Scene、失敗バッチ、書き込み未完了からの再取得を検証。
600回の混合更新では描画を7更新ごとに行う模擬GPUバッファを範囲コピーだけで更新し、
最新SceneのInstance全体とバイト一致を確認した。全体コピーはこの検証用oracleだけに用いる。
Universal-3Dとの回転・ミラー互換性テストも維持。相手側のコード・依存は変更していない。

Regression risk: 旧公開ヘルパー`renderer::upload_ranges`は削除し、新しいSceneの差分APIへ移行。
履歴切れやpacking変更時は全量GPU転送。非常に断片的な更新も全量fallbackする。
全件更新の裾は変動しており、GPU性能改善はまだ確認していない。
WASMビルドとGPU実機統合テストは引き続きPending（GPUテスト1件はignore）。

Keep / Revert: Keep。通常の少数更新で明確なCPU処理量・payloadコピー削減を確認。
全件更新の改善は主張せず、実機profilingとpacking差分化を次の工程として残す。

## 再現

```sh
cargo run --offline --release -p threex-core --example upload_bench
```

旧実装は`benchmarks/upload-baseline-core.zip`に独立crateとして保存してある。
一時ディレクトリに展開し、同じコマンドに`--manifest-path`を付けて実行すると比較できる。
現行crateからこのsnapshotを参照しない。ハッシュと計測条件は`upload-metadata.json`。
Universal-3Dのシンボリックリンクを削除しても、ビルド・テスト・実行に影響しない。
