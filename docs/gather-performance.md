# Compute gather：単一binding版の比較

この資料は分割gather導入前の比較。現在はbinding上限超過も分割して処理する。
[分割gatherの仕様・比較・fallback条件](partitioned-gather.md) を参照。

2026-10-17、macOS aarch64、Rust release／thin LTO。
🟢 [Done] 対応device limits内で、初回・全更新の大量copyをcompute gatherへ集約する。
CPU側のInstance複製、JavaScriptへのフレーム転送、Universal-3Dへの依存は追加しない。

## 経路と互換性

形状bankのsource／destination索引と既存のdirty rangeを使い、最大64の連続copy区間を調べる。
65区間目に達したbankだけ、その回に必要なsource／destinationのpairを作る。
旧方式の100,000個のcopy計画をすべて生成してから判定することは避ける。

| 条件 | 実行 |
| --- | --- |
| 単一形状 | 元Instance bufferを直接描画、bank転送なし |
| 64区間以下・連続したfull更新 | 従来の結合copy |
| 65区間以上、limits内 | bankごとに1 compute dispatch |
| compute非対応・binding／dispatch limits超過 | 従来copyへfallback |
| 変更なし | Instance送信・copy・gatherなし |

10万個のBox／Plane／Sphere交互配置は、初回・全更新とも
`100,000 copy_buffer_to_buffer → 0 copy + 3 compute dispatch`。
別途、task索引のqueue writeが3件必要となる。内部のupload copyやrender命令はこの数に含めない。
描画は引き続き最大3 draws。少数編集、形状変更時のswap-remove、追加・削除、容量再利用、
単一形状から混在への再同期は既存のdirty経路に従う。

shaderは1 threadで1 Instanceの20個のu32を複写し、workgroup sizeは64。
80-byte Instanceのmodel／RGB／geometry tagを演算・型変換せず保持する。
destinationは逆引き表により一意で、bank内で同じslotへ並行書き込みしない。
compute passをrender passより前に置き、wgpuのbuffer usage遷移で描画へ接続する。

索引bufferはbankごとに容量を保持し、bindingには今回のtask長だけを指定する。
容量よりtaskが少なくなった際も古い末尾を実行しない。source／destination bufferの確保容量、
task binding、max buffer size、workgroup数、storage binding数を実device limitsに照合する。
上限を超えた場合はgather用bind／dispatchを作らずcopyへ戻す。

## CPU前後比較

```sh
cargo test -p threex-renderer --release gather_benchmark -- --ignored --nocapture
```

同じGeometryBatchesに対し、gatherを無効にしたcopy-only経路とhybrid経路を比較する。
100,000 InstanceをBox／Plane／Sphereの順に交互配置。3回の独立実行の中央値。
各実行でcopy-only→hybridの順に計時するため、cache・allocator・実行順の影響を含む。
元データ：[geometry-gather.csv](benchmarks/geometry-gather.csv)。

| 計測 | copy-only | hybrid | 1回のbank転送命令 |
| --- | ---: | ---: | --- |
| 初回の計画生成1回 | 0.296 ms | 0.218 ms | 100,000 copy → 3 dispatch |
| 全範囲dirty通知＋計画100回 | 46.866 ms | 38.014 ms | 100,000 copy → 3 dispatch |
| 1 slot dirty通知＋計画1,000回 | 0.0416 ms | 0.0353 ms | 1 copy → 1 copy |

初回計画は構築済みのbankから計画だけを生成する。Scene構築・bank初期化の時間を含めない。
全更新／少数更新は指定dirty rangeによるsyncと計画生成を含む。
`black_box`で計画を保持し、CPUが生成するcopy区間／taskを比較する。
GPU buffer確保、索引のqueue write、bind group生成、command encoding、submit、GPU実行、
表示は計時していない。小さい差分の微小な時間差はノイズを含み、速度向上とは断定しない。

## 追加コストと保留事項

- gather taskは8 bytes/Instance。100k全更新では追加のCPU→GPU送信が800,000 bytes。
- 各Instanceの80 bytesのGPU内再配置は必要なまま。削減対象は主にCPUが発行するcopy命令数。
- 100k交互配置のtask buffer確保容量合計は1.5 MiB。従来のInstance用25 MiBに追加する（driver等は除外）。
- 閾値64は命令数を抑える方針値。実GPUの損益分岐点を測定した値ではない。
- binding上限やworkgroup上限を超える場合、copy命令は多数になることがある。

🟢 [Done] binding上限超過時の分割gather（partitioned-gather.md）。
⭕️ [Pending] 実GPUでのcopy／computeの比較、閾値調整、GPU時間・電力、WASM／browser表示。

## テスト

🟢 [Done] Rust 143件がdebug／releaseで成功（core 52、renderer 14、storage 60、Tauri host 17）。
JavaScript 12件、Clippy全target／feature、format、diff whitespace検査も成功。
Document所有・checkpoint／journal形式・request/session処理は変更していない。
保存中断・破損末尾復旧・再起動後重複排除は既存の回帰テストで維持を確認する。

追加検証はWGSL validation／80-byte Instanceと8-byte taskのlayout、limits境界、対応／非対応経路。
5,000 Instanceへ180回の全更新・大量部分変更・少数編集・形状変更を適用し、全体参照とbyte一致を確認する。
NaN payload・負のゼロも含め、少数変更の最大2件copy、連続fullの1 copy、部分gatherの一意destinationを検証する。
既存3,000回のmembership編集・shrink／regrowthと240回のScene編集の回帰も維持する。

実GPU readback用テストも追加した。

```sh
cargo test -p threex-renderer gpu_gather_readback -- --ignored --nocapture
```

999→65→1000→1 taskで容量を再利用し、workgroup末尾のguard・live binding長・raw word一致を確認する設計。
⭕️ [Pending] 実行を試みたがMetal adapter NotFoundで開始不可。GPUの結果一致は未確認。
CPU模擬コピーとWGSL validationを実機検証の代替とは扱わない。

English: Fragmented transfers above 64 runs use optional compute gather, reducing the 100k interleaved
case from 100k copies to three dispatches plus task uploads. Sparse and coalesced copies and limit fallback
remain intact. GPU execution and performance verification are pending.

简体中文：超过64个复制区间时使用可选compute gather，将10万个交错实例的复制命令合并为3次dispatch，
另需上传索引。保留少量差分复制、连续复制与limits回退。实际GPU执行和性能验证待完成。
