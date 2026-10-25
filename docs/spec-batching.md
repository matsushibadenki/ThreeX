# Custom GeometrySpec batching

2026-10-22。🟢 [Done] 交互に並ぶcustom分割のInstanceを、同じspecにつき1 drawへ集約する。

## 実装

既存のBox／Plane／Sphereの3 GPU bankを維持し、各bank内でGeometrySpecごとにInstance索引を連続配置する。
現在は[境界slotの局所交換](local-spec-packing.md)を使用し、以下の計測はその導入前の歴史的比較。
共有mesh・render pass・80-byte Instance・分割gather・非対応limits時のcopy fallbackは共通のまま。
RustがDocumentとGPUデータを所有し、WebViewへの大容量データ転送は増やさない。
保存v6・hash v2・request protocol v1は変更しない。

通常のTransform／RGB変更では全bankを走査せず、変更slotのみ転送する。
追加・削除・kind／spec交換ではspec境界の代表slotだけを交換し、移動先slotをdirtyにする。
索引のsource→destination対応が変わればrevisionを進め、gather計画とGPU索引を再生成する。
単にtagが変わり索引順序が変わらなければ既存cacheを再利用する。
単一specは元Instance bufferからの直接描画を維持する。

CPUに保持するのは索引とspecで、全Instance複製は追加していない。
構成変更時の一時配列は後続spec境界だけで、全bankのsort・複製は行わない。
初回はspec別に索引を収集する。通常のTransform差分更新では境界の交換も発生しない。

## 比較方法

`batching::tests::custom_spec_benchmark`を同じrelease設定で変更前後それぞれ3回、順番に実行する。
10万Planeを4種類のspecで交互配置し、初回syncと、1 InstanceのRGB変更＋sync＋draw数計算を100回測る。
初回syncにはGPU向けtransfer計画・mesh生成を含めない。CPU時間・draw計画数の比較で、GPU時間ではない。
`docs/benchmarks/spec-batching.csv`に全sampleを保存する。

```sh
cargo test -p threex-renderer --release custom_spec_benchmark -- --ignored --nocapture
```

3回の中央値（同一マシン、他のbuild／benchmarkと同時実行せず）：

| 項目 | 変更前 | 変更後 |
| --- | ---: | ---: |
| 初回sync | 16.335 ms | 29.582 ms |
| RGB差分sync＋draw数計算、100回 | 8.368 ms | 0.008 ms |
| draw数 | 100,000 | 4 |
| 最後の更新で転送するslot数 | 1 | 1 |

初回準備は約81%増え、通常更新のdraw区間走査は大幅に減った。
後者は短い計測なので微秒単位のばらつきを含む。GPU FPSや全frame時間の向上率には換算しない。
この版では1 slotのspec交換でも広いGPU差分転送になり得た。現在の局所交換版は境界slotに限定する。

## 検証と残作業

- 🟢 [Done] workspace debug 153件、renderer release 21件、JS 13件、全target／feature Clippy、fmt、diff whitespace検査。
- 🟢 [Done] 600回の追加・swap-remove・kind／spec交換で全slotのGPU copy結果をbyte単位で照合。
- 🟢 [Done] active spec数とdraw数の一致、全Instanceの重複なし、逆引きslotの整合性。
- 🟢 [Done] 300回のspec交換、gather cache再送／再利用、shrink、単一specへの復帰。
- 🟢 [Done] 通常のRGB編集は1 slot、idleは0、索引revision維持。
- 🟢 [Done] 構成変更時の全bank整列を局所再配置へ置換（[新しい比較・検証](local-spec-packing.md)）。
- 🟢 [Done] 多数の巨大custom meshに対する[resource budget](mesh-budget.md)。
- 🟠 [Next] generation handles。
- ⭕️ [Pending] 実GPUのdraw／gather readbackと時間、WASM／browserの表示。native GPU pipelineテストは実行を試みたがMetal adapter NotFound。

English: Custom instances are grouped into one draw per active GeometrySpec using the existing three GPU banks.
Sparse transform/color uploads and gather fallbacks remain intact. Membership/spec changes reorder affected banks
and invalidate changed index mappings. CPU planning measurements do not establish GPU speedups.

简体中文：自定义实例在原有三种GPU bank内按GeometrySpec分组，每个活跃spec只需一次draw。
保留Transform／颜色的差分上传及gather回退。成员／spec变化时重排受影响bank，并使已变化的索引缓存失效。
CPU计划计时不代表实际GPU加速；频繁结构编辑的局部重排与GPU验证仍待完成。
