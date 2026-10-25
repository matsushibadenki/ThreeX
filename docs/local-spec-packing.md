# Custom specの局所再配置

🟢 [Done] 構成変更時の全bank安定sortを、spec境界slotの交換へ置き換えた。
既存の3 GPU bank・共有mesh・1 specにつき1 draw・差分転送・gather cacheを維持する。

## 配置とcacheの契約

各kindはspec→連続範囲と、開始offset→specの境界表を持つ。
削除時は同じspecの末尾slotを穴へ移し、後続groupの末尾を1つずつ交換して穴をbank末尾へ送る。
既存specへの追加時は逆に後続groupの先頭slotを交換する。新specはbank末尾にgroupを作る。
空のgroupは直ちに除去する。kind交換は旧bankからの削除と新bankへの追加として扱う。

group内のInstance順は固定しない。specのdraw順も固定しない。
現行shaderはopaqueであり、以前の安定sort順をAPI契約にしていない。
逆引きsource→bank offsetを交換のたびに修復し、両方の移動先をdirtyにする。
通常のTransform／RGB変更は1 dirty slot、idleは0のまま。

後続group数をKとすると、1件の構成編集はO(K log K)の境界表操作とO(K)のslot交換・一時領域で済む。
全bank長に比例するsort・索引複製・Instance複製は行わない。
初回だけはspec別に索引を一度収集してbankを構築し、1 Instanceずつの境界交換を避ける。
新しいInstanceを多数追加する場合は1件ごとの境界交換を行うため、spec数が非常に多い場合のbulk最適化は残る。

bank長／kindが変わった場合、または境界slotを交換した場合にrevisionを進める。
同一bank内の交換が最終的に相殺されても、保守的にcacheを無効化する場合がある。
配置が変わらない単独Instanceのtag変更ではrevisionを維持する。
全更新gatherの計画・GPU索引はrevision／buffer size／limitsが同じなら再利用する。
少数差分のcopyはGPU索引を上書きせず、部分gather後の再送・分割不能時のcopy fallbackも維持する。

保存v6・hash v2・request protocol v1・Rust側のDocument所有は変更しない。
Universal-3Dの固定形状Batchを参照したが、リンク先へのbuild／実行時依存は追加していない。

## 同一release設定での比較

変更前の全bank sort版と変更後を、それぞれ3回順番に実行した。
benchmark同士やbuild／Clippyと同時実行していない。
1万／10万Planeを4種類のspecで交互配置し、初回sync、同じsourceのspec交換100回、
追加→削除100組、RGB変更100回を計測する。specは既存の2×3／5×3を往復する。
転送slot数は各syncのdirty destination数を合計したもの。
GPU向けtransfer計画生成・GPU実行・mesh生成・保存処理は時間に含めない。

```sh
cargo test -p threex-renderer --release local_spec_benchmark -- --ignored --nocapture
```

3回の中央値。初回以外は各100回の合計時間：

| Instance数 | 操作 | 全bank sort | 局所交換 |
| ---: | --- | ---: | ---: |
| 10,000 | 初回 | 3.952 ms | 0.688 ms |
| 10,000 | spec交換 | 29.523 ms | 0.044 ms |
| 10,000 | 追加→削除 | 97.849 ms | 0.165 ms |
| 10,000 | RGB変更 | 0.007500 ms | 0.002541 ms |
| 100,000 | 初回 | 50.669 ms | 9.042 ms |
| 100,000 | spec交換 | 373.475 ms | 0.048 ms |
| 100,000 | 追加→削除 | 720.507 ms | 0.265 ms |
| 100,000 | RGB変更 | 0.008417 ms | 0.002500 ms |

| 10万Instanceでのdirty slot合計 | 全bank sort | 局所交換 |
| --- | ---: | ---: |
| spec交換100回 | 5,025,099 | 401 |
| 追加→削除100組 | 12,500,300 | 800 |
| RGB変更100回 | 100 | 100 |

[全sample](benchmarks/local-spec-packing.csv)。短い計測のばらつきやマシン状態の影響を含む。
微秒単位のRGB差を性能保証とはしない。CPU準備時間と転送予定slot数の比較であり、GPU FPSへの換算は行わない。

## 検証

- 🟢 [Done] workspace debug 154件（core 54、renderer 22、storage 61、Tauri host 17）。renderer release 22件。
- 🟢 [Done] JS 13件、全target／feature Clippy、format、diff whitespace検査。
- 🟢 [Done] 600回の追加・削除・kind／spec交換で差分copy結果をbyte単位で照合。group範囲・逆引き・重複なしを検証。
- 🟢 [Done] 10万Instanceで既存spec交換・新spec作成／消滅・append／removeを行い、転送数がspec数に応じた上限以内であることを検証。
- 🟢 [Done] 全削除→再追加、単独Instanceのtag変更時のrevision維持、idle／RGB差分。
- 🟢 [Done] custom specを含む350回のfull／partial／構成変更で、cacheしたGPU索引・分割gather・copyをCPUで模擬しbyte単位で照合。
- 🟢 [Done] 既存の分割limits／fallback、NaN・負のzeroのraw word、journal障害復旧・再起動後の重複排除テスト。
- ⭕️ [Pending] native GPU pipelineテストを試行したがMetal adapter NotFound。実GPU readback・描画・性能、WASM／browserは未検証。
- 🟢 [Done] custom meshの[resource budget](mesh-budget.md)。
- 🟠 [Next] generation handles、多数spec・大量追加時のbulk構成編集最適化。

English: Custom-spec edits now rotate boundary representatives instead of sorting whole banks. Sparse uploads,
three GPU banks, partitioned gather, fallback copies and revision-aware cache reuse remain intact. Release CPU
benchmarks and byte-level transfer simulations pass; GPU rendering/readback and performance remain pending.
No dependency on the Universal-3D symlink was added.

简体中文：自定义spec结构编辑改为交换分组边界实例，不再对整个bank排序。保留三个GPU bank、差分上传、
分块gather、copy回退及基于revision的缓存复用。Release CPU计时与逐字节传输模拟通过；实际GPU显示、
回读和性能仍待验证。未添加对Universal-3D符号链接的依赖。
