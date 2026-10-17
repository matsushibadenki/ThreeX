# Geometry batching：導入時のCPU比較と検証境界

これはcompute gather導入前の形状bankと連続区間方式の比較。現在の転送経路は
[gather-performance.md](gather-performance.md) を参照。以下のcopy-only測定は履歴として保持する。

2026-10-17、macOS aarch64、Rust release／thin LTO。🟢 [Done] 形状別GPU bankの差分計画。
Universal-3Dの3形状batch構成を参照し、ThreeXのScene dirty journalへ接続した。相手への依存は追加しない。

## 再現方法

```sh
cargo test -p threex-renderer --release batching_benchmark -- --ignored --nocapture
```

同じ100,000 Instanceを使い、旧GeometryRunsと新GeometryBatchesを比較する。
初期化は空の計画から構築。新方式は混在時の初回copy計画生成を含む。
更新は1回1 slotの色変更を1,000回行い、各回でdirty処理と全drawの列挙を行う。
新方式は混在時のcopy計画生成も含む。計時はCPUのみで、GPU buffer確保・queue write・
copy／render commandのエンコード・submit・GPU実行・表示は含まない。
`black_box`でdrawとcopy計画を消去されないようにする。

3回の独立実行の中央値。元データ：[geometry-batching.csv](benchmarks/geometry-batching.csv)。
他のプロセス・キャッシュの影響を含む局所計測であり、全アプリの速度倍率には使わない。

| 100k scene／処理 | 旧連続区間方式 | 新形状bank方式 |
| --- | ---: | ---: |
| Boxのみ：初期化 | 0.124 ms | 2.605 ms |
| Boxのみ：1,000回の更新＋draw列挙 | 0.00517 ms | 0.00450 ms |
| Boxのみ：draw数 | 1 | 1 |
| Box／Plane／Sphere交互：初期化 | 4.988 ms | 2.545 ms |
| 同：1,000回の更新＋draw列挙 | 129.186 ms | 0.0414 ms |
| 同：draw数 | 100,000 | 3 |

## 差分更新とコスト

- Sceneの80-byte Instance、dirty range、独立renderer stamp、full-upload fallbackを維持する。
- 単一形状は元GPU bufferを直接描画する。追加copyなし、形状bankは拡張しない。
- 混在時は形状bankを描画し、通常の色／Transform変更は変更Instanceだけ80 bytesをGPU内copyする。
- 形状変更は旧bankのswap-removeによる穴埋めと新bankへの追加で最大2件。ただし容量拡張時は該当bank全体をcopyする。
- 単一形状→混在は全bankを再同期する。初回／全体更新で交互配置の場合は100,000 copy命令になり得る。
- Boxのみの初期化はslot逆引き・bank索引・dirty metadata構築の分だけ遅くなった。毎frameの全Instance走査・CPU複製は追加しない。
- 混在時は元bufferに加えて描画bankを保持する。100k交互配置ではInstance bufferの計画確保量は旧10 MiB→新25 MiB。geometry／depth／driver overheadは含まない。
- GPUはopaque／depth Less。形状ごとにdraw順が変わり、同一depthの重なりの優先順は保証しない。

🟢 [Done] 対応limits内の断片化したcopyをcompute gatherへ置換した。
[実装後の比較とfallback条件](gather-performance.md)。GPU実行の検証は引き続きPending。

## 検証

🟢 [Done] 3,000回の追加・削除・形状／色変更・縮小・再拡張、full-range fallback、
単一／混在切替、bank容量拡張を含むコピーをCPUで模擬し、参照Instanceとのbyte一致を検証。
source逆引き・重複／欠落・最大3 draws、100k Boxの結合copy、1件編集、形状変更、idleを確認する。
既存Sceneの240回の表示／mesh／削除編集でもbank構成を検証する。

Rust workspace：debug／releaseとも140件成功（core 52、renderer 11、storage 60、Tauri host 17）。
保存中断・破損末尾復旧・再起動後request重複排除も既存テストで通過。JavaScript 12件成功。
Clippy全target／feature、format、diff whitespace検査も成功。

⭕️ [Pending] 実GPUテストはMetal adapter NotFound。GPU内copy／描画の実行・映像・GPU時間・電力、
WASM／browserは未検証。CPU計画とWGSL validationの成功を実GPU検証の代替とはしない。

English: Geometry banks cap interleaved scenes at three draws. CPU planning improves for interleaved scenes;
Box startup metadata and extra GPU memory are costs. Single-kind scenes render directly without extra copies.
GPU execution and browser verification remain pending.

简体中文：按形状分组将交错场景限制为最多3次draw，并仅复制变化的GPU实例。
单一形状直接绘制，初始化元数据和额外显存仍有成本。实际GPU执行与浏览器验证待完成。
