# 分割gatherの計画・GPU索引再利用

2026-10-17。🟢 [Done] 全bank更新の計画cache・GPU索引の再利用。
Rust Renderer内で索引とdispatch範囲を保持し、Document／Instanceのsnapshotは複製しない。
Universal-3Dの形状bankとfull／dirty uploadを参照し、シンボリックリンクへの依存は追加していない。

## 再利用契約

GeometryBatchesはbankごとに構成revisionを保持する。
追加・削除・形状変更・swap-removeでsource slot→bank slotの対応が変わるとrevisionを更新する。
色／Transformだけの変更では更新しない。revisionはwrapせず、理論上の枯渇時は停止してABAを避ける。
Sceneのauthoring IDを索引として使わず、既存のpacked slot番号を使う。

Rendererの全体計画cacheのkeyは、構成revision・source buffer容量・bank buffer容量・device limits。
各bankにつき直近の1計画を保持する。単一binding／分割windowの双方に適用する。
強制fullフラグだけでなく、sorted dirty slotがbank全体を覆う場合もO(1)で検出する。
同じkeyならtask配列の生成・window分類・相対化・paddingを繰り返さない。
GPU bufferへのbindingは毎回現在のbufferで作る。GPU bind group自体のcacheではない。

| 更新／状態 | CPU全体計画 | GPU索引 |
| --- | --- | --- |
| 初回・key変更 | 再構築 | 送信 |
| 同じ構成で全bank更新 | 再利用 | 再利用、送信なし |
| 少数copy | 全体計画を保持 | 保持、送信なし |
| 大量の部分gather | 別の一時計画を生成、全体計画は保持 | 部分索引を送信 |
| 部分gather後の全更新 | 同じkeyの全体計画を再利用 | 全体索引を再送 |
| 以降の同じ全更新 | 再利用 | 再利用、送信なし |
| geometry／membership変更 | 変更bankのrevisionで無効化 | 次の全gatherで再送 |
| 新しいRenderer／device | cacheを新規作成 | 初回送信 |

CPU計画の有効性と、GPU索引bufferに現在どの計画が入っているかを分けて管理する。
部分gatherでGPU索引が上書きされても、CPU全体計画の再構築は不要。
索引bufferの再確保時は送信フラグにかかわらず送信する。
既存の「bankごとに1 frameあたり一度prepare→encode→submit」の順序を維持する。

分割不能結果も同じkeyの間は保持して、毎回の分割試行を省く。
結果は従来copyで出力するため、そのcopy区間の生成と発行は引き続き必要。
compute非対応・alignment／binding／dispatch／max buffer sizeによるfallback、
単一形状の直接描画、Sceneのdirty journal・stamp・full-uploadは維持する。
Scene切替でもslotの構成が同じ場合は索引を再利用でき、Instance内容は既存のfull-uploadで更新する。

## 前後の性能比較

```sh
cargo test -p threex-renderer --release cached_partition_benchmark -- --ignored --nocapture
```

macOS aarch64、Rust release／thin LTO、100k個のBox／Plane／Sphere交互配置。
source容量10 MiB、各bank容量5 MiB。cacheとGPU索引の常駐状態を最初に準備し、計時から除外する。
比較対象はcache導入前の分割計画生成と、導入後のprepare。
3回を直列に実行した中央値。元データ：[gather-cache.csv](benchmarks/gather-cache.csv)。
64 KiBは100kで分割を再現する合成limits、128 MiBは単一bindingの対照ケース。
実GPUのlimits／時間測定ではない。

| binding上限／処理100回 | 導入前 | 導入後 |
| --- | ---: | ---: |
| 64 KiB／計画のみ・同じ全bank | 187.792 ms | 0.00958 ms |
| 64 KiB／全bank同期＋転送準備 | 240.121 ms | 54.240 ms |
| 64 KiB／1 slot同期＋転送準備 | 0.00979 ms | 0.00954 ms |
| 128 MiB／計画のみ・同じ全bank | 68.125 ms | 0.00604 ms |
| 128 MiB／全bank同期＋転送準備 | 136.573 ms | 59.989 ms |
| 128 MiB／1 slot同期＋転送準備 | 0.01033 ms | 0.01038 ms |

「全bank同期」はGeometryBatchesへの全範囲dirty通知・geometry tag確認・dirty slot整理を含む。
Sceneの行列計算・Document編集・queue write・bind group／pass生成・command encoding・
submit・GPU実行・表示・電力は含まない。
計画だけのcache hitと、同期を含むコストを混同しない。全体アプリの速度倍率には使わない。
少数変更の微小な差はノイズを含む。各実行で導入前→導入後の順に測るため、
cache・allocator・実行順・他プロセスの影響も含む。

| 同じ構成の全更新1回 | 導入前→導入後 |
| --- | --- |
| 64 KiBのdispatch数 | 369→369 |
| 64 KiBの索引送信計画 | 846,848→0 bytes |
| 128 MiBのdispatch数 | 3→3 |
| 128 MiBの索引送信計画 | 800,000→0 bytes |
| 少数変更 | 1 copy→1 copy、索引送信なし |

初回・key変更・部分gather後の再送はこの「0 bytes」の対象外。
GPU索引の読出しと80 bytes/InstanceのGPU内再配置、変更InstanceのCPU→GPU送信は必要なまま。
実GPU時間・電力の改善は主張しない。

## メモリと残るコスト

CPUに最大3つの全体計画と1つの部分計画を保持する。taskは8 bytes/pair＋alignment padding、
dispatchにはsource／destination／taskの範囲を保持する。100kの64 KiBケースの全体計画は
live task 846,848 bytesと369 dispatch（64-bit環境の範囲データ17,712 bytes）等を保持する。
Vec容量・key・allocator overhead・一時的な再構築メモリはこの値に含めない。RSSの測定値ではない。
GPU索引bufferの保持容量は従来と同じで、100kでは合計1.5 MiB。

初回やmembershipが頻繁に変わる場合はcache missになる。
全範囲更新のgeometry tag確認・dirty slot整理とbind group／compute passの再作成は残る。
cacheは永続化しない。Document形式・request/session protocolは変更しない。

## テスト

🟢 [Done] Rust 147件をdebug／releaseで検証（core 52、renderer 18、storage 60、Tauri host 17）。
JS 12件、Clippy全target／feature、format、diff whitespace検査も成功。
保存中断・破損末尾復旧・再起動後の再送重複排除を含む既存回帰も成功。

追加2テスト：

- 350回の全更新・少数copy・部分gather・形状変更・追加・swap-remove削除でGPU索引常駐を模擬し、参照Instanceとbyte一致。NaN・負のゼロも保持。
- 部分gather後の全体索引再送と、その後の再利用。GPU常駐索引と現在の計画の一致も検証。
- source／bank容量・limits・構成revision変更による無効化、新Rendererの初回送信。
- 分割不能結果の再利用とcopy fallback、compute非対応経路。

GPU readbackテストも、索引送信後にsourceだけを更新し、索引を送信しない2回目のgatherを
確認するよう拡張した。分割／通常binding、999→65→1000→1 taskの容量再利用と末尾も対象。

```sh
cargo test -p threex-renderer gpu_gather_readback -- --ignored --nocapture
```

⭕️ [Pending] 実行を試みたがMetal adapter NotFound。索引再利用の実GPU結果一致、時間・電力、
WASM／browser表示は未検証。CPU模擬とWGSL validationを実GPU検証の代替とは扱わない。

English: Full-bank gather plans and GPU indices are reused while topology, capacities and limits match.
Partial copies preserve index residency; partial gathers require one full-index re-upload later.
CPU preparation and planned index uploads decrease in warm cases. GPU verification remains pending.

简体中文：当bank结构、容量和limits一致时，复用完整gather计划与GPU索引。
少量copy保留索引，大量部分gather覆盖索引后，下次完整更新需重新上传一次。
稳定结构下CPU准备和索引上传计划减少，实际GPU验证仍待完成。
