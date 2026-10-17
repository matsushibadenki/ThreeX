# Binding上限超過時の分割gather

この資料は計画cache導入前の分割gatherと単一binding方式の比較。現在の計画・索引再利用は
[gather-cache.md](gather-cache.md) を参照。

2026-10-17。🟢 [Done] aligned binding window・task／dispatch分割。
Rust所有のInstance、形状別bank、少数変更の差分copy、非対応時のfallbackを維持する。
Universal-3Dの形状bankとfull／dirty uploadの分岐を参照し、実装・依存はThreeX内に保持した。

## 分割方式

単一bindingに収まる場合は従来どおり1 bank＝1 dispatch。
収まらない場合は、同じsource／destination GPU bufferを複数のwindowとしてbindingする。
中間Instance bufferや全InstanceのCPU複製は追加しない。80-byte Instanceとshaderは変更しない。

`A = min_storage_buffer_offset_alignment` とすると、windowのInstance数は
`floor(binding_limit / 80 / U) × U`、`U = A / gcd(A,80)`。
これにより各windowの先頭はA-byte alignmentとInstance境界の両方に一致する。
最後のwindowはbuffer末尾で切り詰める。各taskのsource／destination番号をwindow内の相対番号へ変換する。

同じsource／destination windowの組をまとめ、次にtask bindingサイズと
`max_compute_workgroups_per_dimension × 64` の小さい方で分割する。
入力が同じwindow内に続く間は、除算・hash lookupを繰り返さない。
window組の順序はsortして決定する。逆引き表によりdestinationは一意で、分割後も各taskを一度だけ実行する。

全chunkのtaskをalignment padding付きの1 bufferへ詰め、bankごとにqueue writeを一度行う。
各dispatchは自分のtask範囲だけをbindingする。同じtask bufferへchunkごとにqueue writeする方式は使わない。
後の書き込みで先のdispatch用索引が上書きされることを避ける。
compute passはdispatchごとに分け、最後に既存のrender passへ接続する。

## 差分経路とfallback

| 状態 | 経路 |
| --- | --- |
| 単一形状 | 元Instance bufferを直接描画 |
| 少数・連続したcopy区間 | 既存の結合copy。window計画を作らない |
| 65区間以上・単一binding内 | 従来の1 bank＝1 dispatch |
| binding／dispatch上限超過 | window／task分割gather |
| compute非対応、window構築不能、padding込みtask bufferがmax buffer size超過等 | 従来copyへfallback |
| 変更なし | Instance転送・gatherなし |

source／destination自体はmax buffer size以内である必要がある。
windowが1 Instanceも含められない場合や、不正な索引を受け取った場合は計画を返さない。
padding込みtask bufferが上限を超えた場合も、GPUに部分的な計画を送信する前にcopyへ戻す。
Scene切替・履歴切れ・bank容量拡張・単一／混在切替のfull更新条件は維持する。
Document・checkpoint／journal・request/session protocolは変更していない。

## 前後のCPU計画・命令数

```sh
cargo test -p threex-renderer --release partition_benchmark -- --ignored --nocapture
```

macOS aarch64、Rust release／thin LTO、100,000個のBox／Plane／Sphere交互配置。
source確保容量10 MiB、各bank容量5 MiB。構築済みGeometryBatchesから100回の転送計画を生成する。
全更新は各bank全体、少数更新は1 slotのdirty range。sync・Scene構築時間は含めない。
旧経路は単一bindingのfits判定を使い、上限超過時はcopyへfallback。
新経路は同じ入力で分割計画を生成する。3回の独立実行の中央値。
元データ：[partitioned-gather.csv](benchmarks/partitioned-gather.csv)。

64 KiBは100kのままで上限超過を再現するための**合成limits**。実deviceの計測値ではない。
128 MiBはbinding内に収まる対照ケース。実際の128 MiB境界越えは別のunit testで索引を検証する。

| binding上限／変更 | CPU計画100回：旧→新 | 1回の再配置命令：旧→新 |
| --- | ---: | --- |
| 64 KiB／全更新 | 25.231→57.470 ms | 100,000 copy→369 dispatch |
| 64 KiB／1 slot | 0.00346→0.00292 ms | 1 copy→1 copy |
| 128 MiB／全更新 | 20.472→23.832 ms | 3 dispatch→3 dispatch |
| 128 MiB／1 slot | 0.00383→0.00296 ms | 1 copy→1 copy |

命令数は削減したが、window分類・相対化・padding・境界検証のためCPU計画は増加した。
全体の高速化を達成したとは扱わない。微小な少数変更の時間差もノイズを含む。
copy-only→新経路の順に計測しており、cache・allocator・実行順の影響を含む。

GPU buffer確保、taskのqueue write、bind group／compute pass作成、command encoding、submit、
GPU実行・表示・電力は測定に含まない。命令数はAPIから発行するcopy／dispatchの数であり、
queue write内部のupload copyやdraw等は含めない。

## 追加コストと次の優先項目

- 64 KiBの全更新でtask送信はpadding込み846,848 bytes／回、bankごとのqueue writeは3件。
- 128 MiBでは従来gatherと同じ800,000 bytes／回。少数変更はtask送信なし。
- task bufferは容量を再利用する。100kの両ケースでは合計1.5 MiB確保で、source／bankの確保量は変わらない。
- GPU内で必要な80 bytes/Instanceの再配置自体は減らしていない。
- window組が細かく散る場合はdispatch数・paddingが増える。max buffer sizeを超える場合はcopyへ戻す。

🟢 [Done] 分割計画・索引の再利用で全更新時のCPU分類・索引送信を削減した。
[実装後の比較・無効化条件](gather-cache.md)。

## テストと未検証範囲

🟢 [Done] Rust 145件がdebug／releaseで成功（core 52、renderer 16、storage 60、Tauri host 17）。
JS 12件、Clippy全target／feature、format、diff whitespace検査も成功。
保存中断・破損末尾復旧・再起動後の再送重複排除を含む既存回帰も成功。

追加2テストは次を検証する。

- alignment 16／64／256／512、両bufferのwindow境界・端数、相対索引と入力taskの完全一致・一意destination。
- 実際の128 MiB境界越えを含む大きなbufferの計画（大容量bufferは確保しない）。
- workgroup上限1による300 task→5 dispatch、task offset alignment・live binding長・bindingサイズ。
- 90回の全更新・少数変更・形状変更・swap-remove削除をCPUで模擬し、NaN・負のゼロを含め参照Instanceとbyte一致。
- 非対応compute、不正索引、window構築不能、padding込み容量超過時のfallback。

GPU readbackテストも、小さい合成limitsによる分割と通常bindingを切り替えるよう更新した。
999→65→1000→1 taskの容量再利用・workgroup末尾・古いtask末尾の非実行を確認する設計。

```sh
cargo test -p threex-renderer gpu_gather_readback -- --ignored --nocapture
```

⭕️ [Pending] 実行を試みたがMetal adapter NotFoundで開始できない。実GPUでの結果一致、GPU時間、
電力、WASM／browser表示は未検証。CPUのbyte一致とWGSL validationを実GPU検証とは扱わない。

English: Oversized bindings use aligned source/destination windows and split task dispatches. Sparse copies
and unsupported-device/size fallback remain intact. A synthetic 64 KiB case reduces 100k copies to 369 dispatches,
but CPU planning increases. GPU execution and overall speed remain unverified.

简体中文：超过binding上限时使用对齐的源／目标窗口并拆分task与dispatch，保留少量差分复制和fallback。
合成64 KiB测试将10万次copy降为369次dispatch，但CPU规划耗时增加。实际GPU执行与整体性能仍待验证。
