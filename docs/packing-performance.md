# Instance集合の差分更新と性能比較

2026-10-11。表示・非表示、Meshフラグ変更、Entity追加・削除による全体再パッキングを廃止。
描画対象を密な配列で保持し、追加は末尾へ、削除は末尾要素を空き位置へ移動する。
移動・追加・Transform/color変更の位置だけをrevision journalへ記録する。
末尾削除はデータ転送なしでdraw countだけが変わる。描画先ごとのstampは維持。
履歴の範囲を現在のInstance数で切り詰め、描画を飛ばした間の縮小でも範囲外を参照しない。

## 契約と制約

`instances()`は借用スライス。従来のID順は保証せず、Entity IDから`instance_index(id)`で
現在の位置を取得する。非表示・Meshなし・削除済みはNone。位置は変更後に再取得する。
不透明・depth付き描画を対象とし、同深度で重なる面の優先順は保証しない。
透明描画やrenderOrderを追加する際は別のソート段階が必要。
Universal-3Dの可視性・変換処理を参照したが、相手リポジトリは変更せず依存も追加していない。

集合更新自体は変更対象数に比例する。親の表示変更は子孫の可視性評価が必要。
Entity生成・削除・reparentの全体階層検証／隣接表再構築は残り、完全なO(変更数)ではない。
逆引きpacked_slotsは64bit環境で容量100kあたり約0.8 MB（Vec余剰容量は別途）増える。
メモリRSSの比較は今回行っていない。GPU容量増加・Scene切替・履歴切れ・過度の断片化は全量同期。

## 計測方法

macOS 27.0.1、arm64、rustc 1.99.0、release thin LTO、offline。
100 / 1k / 10k / 100k件、各操作12回warmup・128回測定、前後2組を実施。
時間はScene.apply + 差分送信計画 + 借用Instanceのblack_box。コマンド生成・初期構築は除外。
表示／Meshフラグ、Entity削除・再生成は同じIDを交互に切り替える。
skip_8は8個の異なるIDを個別apply後、一度だけ同期。allは全件表示を交互に切り替える。
送信量はrange長×80 Bの予定値で、GPUqueue、再確保、GPU描画時間は含まない。
CSVのmean_instance_writesは各iteration最後のapplyの値（skip_8の全8回の総和ではない）。

## 100k件での結果

最初の前後組、時間はp95。生CSVにはmedian・p99・maxと他サイズも保存。

| 操作 | 前 ms | 後 ms | 平均送信予定量 前 → 後 |
| --- | ---: | ---: | ---: |
| 1件表示切り替え | 0.558000 | 0.000250 | 7,999,960 → 40 B |
| 1件Meshフラグ切り替え | 0.491042 | 0.000250 | 7,999,960 → 40 B |
| 1件Entity削除／再生成 | 4.187208 | 3.210125 | 7,999,960 → 40 B |
| 末尾表示切り替え | 0.535084 | 0.000291 | 7,999,960 → 40 B |
| 8更新を飛ばして同期 | 3.752833 | 0.001750 | 7,999,680 → 320 B |
| 全件表示切り替え | 21.816042 | 20.804375 | 4,000,000 → 4,000,000 B |

最初の任意位置削除は1件移動（80 B）、再表示は1件追加（80 B）。その後は末尾削除0 Bと
末尾追加80 Bの交互になるため、warmup後の平均は40 B。毎回任意位置削除が40 Bという意味ではない。
GPUバッファの再確保が必要な追加では全量転送になる。

2組目のp95は表示0.497042→0.000334 ms、Mesh0.585667→0.000292 ms、
Entity3.684042→3.402625 ms、全件20.647625→20.938375 ms。
全件更新は2組目で約1.4%悪化し、改善は主張しない。100件の全件変更も最初の組で
0.015291→0.019750 ms（約29%悪化）。swap-removeの移動作業が増える一方、
100k全件切り替えの送信量は減らない。局所変更の改善を目的とする実装。

## 検証・再現

17個のcoreテスト、WGSL検証1個、JS互換4個が成功。debug/releaseとClippy・formatを確認。
600回の混在変更を独立した全量参照モデルとEntity IDで比較。親子表示、reparent、
空Scene、slot再利用、100k件の1件移動、独立した描画先、縮小をまたぐ履歴、retry・無効batchを検証。
GPU実描画・WASM実機比較は ⭕️ [Pending]。GPU性能を計測した結果ではない。

```sh
cargo run --offline --release -p threex-core --example packing_bench
cargo test --offline --workspace
cargo test --offline --release --workspace
cargo clippy --offline --workspace --all-targets -- -D warnings
node --test tests/compat/scene.test.js
```

変更前のcoreと同じベンチを`benchmarks/packing-baseline-core.zip`に独立crateとして保存。
一時ディレクトリに展開し、`cargo run --offline --release --manifest-path <展開先>/Cargo.toml --example packing_bench`で再現。
生データは`benchmarks/packing-before.csv`・`packing-after.csv`・`packing-repeat-{before,after}.csv`。
比較表CSVと環境・測定条件JSONも同じディレクトリに保存。Universal-3Dへのリンクは不要。

- 🟢 [Done] 集合変更の差分パッキングと前後CPU比較。
- 🟠 [Next] Entity構造変更の全体検証・隣接表再構築の差分化。
- ⭕️ [Pending] 実GPU転送・描画順・WASMの実機検証。
