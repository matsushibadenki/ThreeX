# Universal-3Dとの協調開発

2026-10-11時点の両プロジェクトの実装を確認した方針。
Universal-3Dは制作アプリ、ThreeXはThree.js互換の描画エンジンを担当する。
参照用シンボリックリンクはビルド・実行・テスト・配布の依存に含めない。
関連プロジェクトのコードを変更する際は、相手側の作業内容と検証結果を確認する。

## 境界と互換性

| 項目 | Universal-3D | ThreeX | 将来の接続方針 |
| --- | --- | --- | --- |
| 所有者 | Rust Editor / Document | Rust Scene / Renderer | Documentから描画用データを抽出。UIに所有させない |
| GPU | wgpu 27 | wgpu 27 | 同じメジャーを基準に更新を検証 |
| ID | 文書内u64、JS安全整数範囲 | 実行時u32、再利用なし | 文書IDと実行時ハンドルの対応表をアダプターが管理 |
| 回転 | 度、軸順にCPU評価 | ラジアン、XYZ Euler | 単位だけでなく合成順序を変換テストで確認 |
| 形状 | cube / sphere / plane | BoxGeometryのみ | 対応していない形状を暗黙に立方体へ変換しない |
| ミラー | ワールドX反射 | 未対応 | インスタンス変換として評価、負スケールも検証 |
| 保存 | .u3d version 1 | 永続文書形式なし | エンジンに編集履歴・選択・パネル配置を持ち込まない |
| 更新 | 操作時再描画 | 操作時再描画、dirty batch | 静止時の常時描画を避ける |

## 採用した改善

Universal-3Dの描画面復旧処理を参考に、ThreeXのWASM Runtimeにも
SurfaceError::Lost / Outdated時の再configureと一度の再取得を追加した。
再取得失敗とTimeout等は呼び出し元へ返し、無限再試行を行わない。
WASM環境とGPUが利用できないため、この経路の実機検証は保留。

## 進捗

- 🟢 [Done] 両プロジェクトのデータ所有、描画構造、wgpuバージョンを確認。
- 🟢 [Done] シンボリックリンクへの依存を追加せず協調方針を記録。
- 🟢 [Done] 原点・非一様スケール・複数軸回転・特異角・負スケール・ワールドミラーの変換テスト。
- 🟢 [Done] universal_rotationでUniversal-3Dの逐次X/Y/Z回転をThreeXのXYZ Eulerに変換。
- 🟠 [Next] 同じ変換fixtureをUniversal-3D側でも実行して相互検証。
- 🟠 [Next] ThreeXのカメラ／形状機能を整えてから描画アダプターを設計。
- 🔴 [Later] 安定したエンジンAPIをバージョン付きパッケージとして利用。
- ⭕️ [Pending] 両レンダラーでの描画比較、WASM Surface復旧の実機検証。

共通化する場合は公開パッケージまたは各リポジトリ内に独立したコードを配置し、
参照用のローカルパス依存、外部ファイルへのinclude、symlinkを導入しない。

ThreeX側のテストは確認済みのUniversal-3Dの変換式を独立した参照式として保持し、
相手側のファイルをincludeせず検証する。Universal-3D側のコード・roadmapは変更していない。

## SoA・差分Transform実装での互換性確認

2026-10-11にUniversal-3DのDocumentと逐次X/Y/Z回転・ワールドXミラー式を再確認。
ThreeXのコマンド形式、Entity ID、XYZ Euler単位、親子合成、ID順の描画データ順序は維持。
変換済みの回転・非一様／負スケール・ワールドミラーをSoAシーンに登録し、差分変更後の
ワールド行列を相手側のCPU式を使った独立参照値と比較した。特異角も含めテスト成功。
相手側のコードをビルド時に参照せず、相手リポジトリには変更を加えていない。

Rustアダプターを作る際は、`Scene::instances()`が保持データの借用スライスを返す点に注意する。
編集Documentの選択・履歴・パネル配置は引き続きUniversal-3D側が所有し、ThreeXは描画状態のみ所有する。

- 🟢 [Done] 新しいSoA・ワールド差分更新経路で変換互換性を確認。
- ⭕️ [Pending] 両プロジェクトの実描画比較とGPU性能比較。

## 描画先ごとの差分同期

Universal-3Dの逐次X/Y/Z回転とワールドミラー式を再確認し、既存の変換契約を維持。
今回も相手側のソースをビルド時に参照せず、シンボリックリンクへの依存は追加していない。
将来の複数ビューポートでは、Rust Sceneが差分履歴を保持し、各Rust Rendererが
最後にGPUへ同期したSceneStampのみを保持する。WebViewへInstance／頂点／フレームデータを送らない。
文書・編集状態はUniversal-3D側が所有する。

各描画先は同じ差分履歴を独立に読める。描画を飛ばした更新は統合され、履歴を超えて
遅れた描画先は最新データを全量転送する。同じrevisionの別Sceneも識別する。
GPUバッファを作り直す場合は`force_full = true`とし、queue書き込み登録後にstampを更新する。
相手リポジトリには変更を加えていない。両アプリの実描画比較は引き続きPending。

- 🟢 [Done] 独立した描画先の差分同期、取りこぼし防止、Scene切り替えのCPU検証。
- ⭕️ [Pending] 複数GPUビューポートでの実機統合・性能検証。

## Instance集合の差分更新

Universal-3DのDocument可視性フィルタと描画時の変換式を参照し、今回も相手側のコードは変更せず、
ビルド・実行・テストの依存を追加していない。変換・Entity ID・親子可視性の契約は維持する。
ThreeXの描画配列は密なappend/swap-remove方式になり、従来のID順という内部順序は廃止した。
アダプター・pickingはEntity IDを保持し、必要時に`instance_index(id)`で現在の描画位置を取得する。
削除・非表示によって別Entityの描画位置が変わるため、配列位置を永続IDとして扱わない。
現在の不透明・depth付き描画ではソートを行わない。重なる同深度面の優先順は保証しない。
将来の透明描画・明示的renderOrderには独立したソート段階が必要。

- 🟢 [Done] 集合変更・親子表示・slot再利用・描画を飛ばした縮小のCPU同期検証。
- 🟢 [Done] Universal変換参照テストは集合差分化後も成功。
- ⭕️ [Pending] 描画順変更を含むGPU実描画比較。

## Document保存・復旧

2026-10-12にUniversal-3Dのversion=1検証、一時JSON保存→rename、保存中の文書比較によるdirty維持を確認。
ThreeXはその所有境界を維持し、固有のbinary checkpoint v1と不変世代・CRC32・履歴復元を独立実装。
.u3d互換の読込ではない。変換／単位／ID差を解決するimportアダプターは別工程。
背景保存はRust側PreparedSave、保存完了はSceneStampで判定。WebViewへ全量を送らない。
相手側ソース・roadmapは変更せず、symlink・外部include・ローカルpath依存を追加していない。

- 🟢 [Done] macOSでversion・履歴・世代rollback・子プロセス終了の保存復旧テスト。
- ⭕️ [Pending] 両アプリの文書交換、実UI、電源断とOS別durability。

## 自動保存journal

2026-10-12にUniversal-3DのRust editor dirty管理と明示save経路を再確認。
ThreeXにRust AutosaveDocument所有者と独立した命令journalを実装し、UI状態所有は増やしていない。
Universal-3D側へソース・roadmap変更は加えず、参照symlinkへの依存も追加していない。
.u3dとの交換形式や相手側autosaveの実装を保証するものではない。

- 🟢 [Done] 未保存取引・Undo/Redo・破損末尾・append／compaction中断のCPU復旧検証。
- ⭕️ [Pending] 両アプリの実UI接続、実電源断とOS別durability。

## 差分transaction staging

2026-10-12にUniversal-3DのRust-owned編集とdirty/save境界を再確認。
ThreeXは変更IDのPreparedTransactionを検証し、journal同期後に同じSceneへ適用する。
変換・ID・Undo/Redo契約と相手側の独立参照テストは維持。相手のコードは変更せず、
シンボリックリンク・外部include・相手リポジトリのpath依存は追加していない。

- 🟢 [Done] 全Document複製廃止、Scene identity維持、無効・stale・別Document拒否、性能比較。
- ⭕️ [Pending] 実UI接続・実電源断・GPU描画比較。

## Group commit

2026-10-12にUniversal-3DのRust所有・保存中のdirty維持を再確認。ThreeX側は複数の編集recordを1sync後に公開し、個別revision／Undoを維持する。相手側コードを変更せず、symlink依存も追加していない。

- 🟢 [Done] group commit／正常prefix復旧とCPU＋IO計測。
- ⭕️ [Pending] 実UI・OS別durability。

## Group prefix差分化

2026-10-12にUniversal-3DのRust文書所有と保存中のdirty維持を再確認。ThreeX内で変更ID overlayを実装し、先行編集の状態を後続検証へ引き継ぐ。ID・変換・Undo・復旧の契約を維持し、相手側コードは変更していない。symlink依存を追加していない。

- 🟢 [Done] 依存編集・削除再作成・混在履歴の逐次参照検証と性能比較。
- ⭕️ [Pending] 両アプリ実UI・実電源断・OS別durability。

## 構造編集の差分化

2026-10-12にUniversal-3DのRust文書所有と保存中のdirty判定を再確認。ThreeXの変更祖先経路検証と辺の差分更新を独立実装。親削除・子付け替え・slot再利用・Undoの契約を維持し、journal末尾の正常transaction復旧を確認。相手側ソース変更、symlink／外部include依存は追加していない。

- 🟢 [Done] 構造編集差分化とCPU／同期込み性能比較。
- ⭕️ [Pending] 相互文書交換・実UI・OS別durability。

## Rust worker接続

2026-10-14にUniversal-3DのRust App State／Editor所有とsaveのdirty判定を参照。
ThreeXはウインドウ非依存のworker clientを用意し、Document／journal正本は専用Rust workerで所有する。
コマンドと小さい状態のみをメッセージ化し、Instance／frame全量をUIへ送る経路は追加していない。
相手側ソース・roadmapは変更せず、symlinkや相手repositoryへのpath依存を追加していない。

- 🟢 [Done] native Rust workerとsession内再送重複排除のheadless検証。
- ⭕️ [Pending] Tauriウインドウ・wgpu Rendererへの実機接続。

## 永続IDと再送protocol

2026-10-14にUniversal-3Dの文書versionと文書内u64 Object IDを参照。ThreeXは別の128bit Document／Session IDを導入し、文書内Object IDとは区別する。変換式・既存Object ID・Rust所有は維持。相手repositoryへのコード変更・symlink依存は追加していない。.u3dとの自動交換や相手側request protocol互換は未実装。

- 🟢 [Done] 文書IDの永続化とnative restart後の同一request再送抑止。
- ⭕️ [Pending] 両アプリ実UI・文書交換・Tauri protocol接続。

## ProtocolRequest group commit

2026-10-14にUniversal-3Dの`save_document`を再確認。Rust Editorのsnapshotを保存し、保存完了時に
現在Documentとの一致を確かめる設計を参照した。ThreeXでは単一Rust ownerを維持し、groupの編集とreceiptを
同期後に公開する。UIへframe／Instance配列を送らず、Document ID・revision・receiptと小さい状態を返す。
.u3d形式やUniversal-3Dのコードは変更せず、相手側にprotocol互換性があるとは仮定しない。

- 🟢 [Done] Protocol Commit group、worker自動batch、再起動後のprefix重複排除・同期回数／性能比較。
- ⭕️ [Pending] 相手アプリを含む実UI／文書交換の統合検証。

シンボリックリンクは参照のみ。import、include、Cargo path、実行時アクセスへの依存を追加していない。
詳細：[four/protocol-group-performance.md](four/protocol-group-performance.md)。

## worker候補prefix差分準備

2026-10-15にUniversal-3DのRust Editor／非同期保存時のsnapshot一致判定を再確認。
ThreeXは単一Rust ownerで未公開の候補差分を保持し、Scene・request metadataの一致をIO前に確認する。
UIへは命令・小さいstate／receiptのみを返し、GPU／frame全量の通信を追加していない。
相手側ソースは参照のみで変更していない。相手の保存形式／request protocolとの直接互換性は仮定しない。

- 🟢 [Done] worker ProtocolRequest候補差分・再送／復旧維持・前後性能比較。
- ⭕️ [Pending] Universal-3Dとの実UI／文書交換統合検証。

シンボリックリンクへのCargo path・import・include・実行時依存は追加していない。
[仕様と実測](four/worker-prefix-performance.md)。

## 裸Commit worker候補差分

2026-10-15にUniversal-3Dの非同期保存時のRust Editor snapshot一致判定を参照。
ThreeXはRust Document所有を維持し、受理済みcommand／Undo差分から直接journalを生成して
Scene identity／revisionをIO前に検証する。ウインドウへの通信は命令・小さいstateの契約のまま。

- 🟢 [Done] 裸Commit候補差分・live-worker再送cache／復旧維持・前後性能比較。
- ⭕️ [Pending] Universal-3Dとの実UI／文書交換統合検証。

相手側ソースは参照のみ。相手保存形式／protocolとの直接互換性は仮定せず、
シンボリックリンクへのCargo path・import・include・実行時依存は追加していない。
[仕様・性能](four/bare-prefix-performance.md)。

## 意味内容hash

2026-10-15。Universal-3DのRust Editorによる保存snapshot比較を参照し、ThreeXでも保存captureに
現在の意味内容hashを結び付けた。SHA-256一致だけでdirty／revision／receipt guardを置換していない。
相手の.u3d serializationとのhash一致は仕様に含めず、ThreeXの正規化schema v1を明示する。

- 🟢 [Done] 意味内容hash、保存receipt・worker照会・復旧検証。
- ⭕️ [Pending] 相手アプリを含む実UI／文書交換検証。

相手ソースは参照のみ。シンボリックリンク依存を追加していない。
[仕様](four/content-hash.md)。

## session lifecycle

2026-10-15。Universal-3DのRust Editor所有とsnapshot保存・dirty判定の境界を参照。
ThreeXのsession終了は文書属性を変更せず、Rustの永続request metadataで管理する。
window閉鎖／通信切断を勝手に終了と解釈せず、明示EndSessionを単一workerで処理する。
相手側に同protocolを仮定せず、相手ソース／保存形式を変更していない。

- 🟢 [Done] 永続session終了とActive枠再利用、終了後／再起動後の再送抑止。
- ⭕️ [Pending] 実UIの切断／終了・文書交換との連携。

シンボリックリンクへのCargo path・import・include・実行時依存は追加していない。
[仕様・復旧検証](four/session-lifecycle.md)。

## 永続エラー結果

2026-10-15にUniversal-3DのRust Editor所有、revision／base_revisionとDocumentPatchの差分通知、
保存snapshot一致判定を再確認。ThreeXはScene revisionと永続request metadataを区別し、失敗は
Sceneを変更せずRust側で記録する。再送のエラーrevisionは失敗当時の値であり、現在stateは別途照会する。
相手側protocolとの互換性を仮定せず、相手ソース・保存形式を変更していない。

- 🟢 [Done] 受理済み編集エラー結果・元の結果の再送、v4移行と障害復旧検証。
- ⭕️ [Pending] 実UI／文書交換・Tauri IPCの統合検証。

シンボリックリンクへのCargo path・import・include・実行時依存は追加していない。
[仕様・検証](four/durable-errors.md)。

## Tauri async／IPC境界

2026-10-15。Universal-3DのRust Editor所有、revision／base_revisionによるDocumentPatch、
保存snapshot比較を参照。ThreeXもウインドウから状態所有を分離し、単一Rust workerへ
小さい命令・状態のみを渡す。相手側のprotocolやwindowラベルを互換契約とはしていない。

- 🟢 [Done] Tauri async IPC、window／origin／session認可、3言語エラーDTO、restart再送検証。
- ⭕️ [Pending] 実Wry UI／相手アプリとの文書交換・Renderer接続。

相手ソースは参照のみ。シンボリックリンクへの依存は追加していない。
[仕様と検証](four/tauri-ipc.md)。

## Rust Rendererとnative surface lifecycle

2026-10-15。Universal-3Dのopen_viewportにおけるイベントループ上のWindow／Surface生成と
Rust Editor／Renderer所有分離を参照した。ThreeXではDocument workerの単一所有を保持し、
同queueのRender命令でSceneを直接借用する。surfaceの交換とGPU障害を文書編集・永続receiptから分離した。
相手のコード・保存形式・protocolを変更せず、相手のwindowラベルを互換契約にしていない。

- 🟢 [Done] native backend・世代付き交換／破棄・有界復旧、worker借用とrestart再送検証。
- ⭕️ [Pending] 実OS／GPU surface表示・両アプリを使うUI／文書交換。

シンボリックリンクへのCargo path／include／import／実行時依存は追加していない。
[仕様と検証境界](four/native-renderer.md)。

## Nativeイベント配線とon-demand redraw

2026-10-15。Universal-3Dのnative viewport resize時のdrawとDestroyed時のRenderer解放を参照。
ThreeXではnative callbackを世代付きの小さいintentへ変換し、単一Document workerの永続公開後に
schedulerへwakeを送る。DocumentやSceneをwindow／schedulerへ複製せず、再送は永続protocolで処理する。
相手の保存形式・request protocolに互換を仮定せず、相手ソースを変更していない。

- 🟢 [Done] callback自動配線・イベント集約・backpressure保持とon-demand scheduler、restart再送検証。
- ⭕️ [Pending] 実OS／GPUのイベント配送・表示と両アプリの文書交換。

シンボリックリンクへのCargo path／include／import／実行時依存は追加していない。
[仕様・検証境界](four/native-redraw.md)。

## Perspective camera

2026-10-16。Universal-3Dのcamera_basis／camera_matrixにおけるeye／target／upと
右手系の視点構築を参照した。ThreeXは既存XYZ radians／親TRSとWebGPU depth 0..1を維持し、
Rustの可逆affine view計算へ接続した。相手のorbit distance／pitch／yawや保存camera形式を
ThreeXの互換契約へ流用していない。相手ソースは参照のみ。

- 🟢 [Done] camera回転・親TRS・zoom・world lookAtとCPU検証。
- ⭕️ [Pending] 実GPU／browser／相手アプリを含む視点・文書交換検証。

シンボリックリンクへのCargo path／import／include／実行時依存は追加していない。
[仕様と検証境界](camera.md)。

## Plane／Sphereと差分描画

2026-10-16。Universal-3Dのshared cube／sphere／plane生成と形状別描画を参照した。
ThreeXはThree互換のXY中心Plane、radius=1のSphereを採用し、相手のXZ plane／radius=.5 sphereとは
単位形状を区別する。相手形式の直接importや同じgeometry IDの互換を仮定しない。
ThreeXの既存Box vertex/index・64-byte wire・80-byte Instanceを保持し、形状のみのdirty更新を追加した。

- 🟢 [Done] shared geometry、差分描画区間、保存形式v5・hash v2、旧形式移行・復旧・再送検証。
- 🟢 [Done] 形状の交互配置でも最大3 drawsへ抑えるbatching（2026-10-17）。
- ⭕️ [Pending] 実GPU／browser表示・相手アプリとの文書交換。

相手ソースは参照のみ。シンボリックリンクへのimport／include／Cargo path／実行時依存は追加していない。
[仕様と検証境界](geometry.md)。

## 形状別GPU batching

2026-10-17。Universal-3Dの `Batch`／`GpuScene.batches` と形状別差分uploadを参照した。
ThreeXも3形状bankを使うが、相手のCPU Instance cacheを流用せず、Sceneのdirty rangeと
source slot逆引きからGPU内コピーを生成する。Document・Sceneの所有と保存／再送処理は維持する。

- 🟢 [Done] 最大3 draws、追加／削除／形状変更の差分bank更新、CPU模擬コピー検証と前後比較。
- 🟢 [Done] 対応limits内の交互配置の初回・全更新をcompute gatherへ集約。
- ⭕️ [Pending] 実GPU／browser性能・表示検証。

相手ソースは参照のみ。シンボリックリンクへのimport／include／Cargo path／実行時依存は追加していない。
[性能比較と制約](batching-performance.md)。

## 断片化したcopyのcompute gather

2026-10-17。Universal-3Dの形状別bank、full uploadとdirty rangeの分岐を再確認した。
ThreeXはRust側のInstance所有と既存差分経路を維持し、相手のCPU Instance複製を導入せず、
大量の非連続copyだけをcompute gatherへ切り替える。相手の実装・保存形式を直接共有しない。

- 🟢 [Done] 64 copy区間を超えるbankのgather、device／binding／dispatch limits検査とcopy fallback。
- 🟢 [Done] 全更新／大量部分変更／少数編集のCPU byte検証・命令数とrelease前後比較。
- 🟢 [Done] binding上限超過時の分割gather（partitioned-gather.md）。
- ⭕️ [Pending] 実GPU readback・時間・電力・browser検証。Metal adapter NotFound。

シンボリックリンクへのCargo path／import／include／実行時依存は追加していない。
[仕様と性能比較](gather-performance.md)。

## binding上限超過時の分割gather

2026-10-17。Universal-3Dの形状bankとfull／dirty uploadの分岐を再確認した。
ThreeXは相手のCPU Instance cacheを依存させず、Rust所有のInstanceと差分索引を維持する。
大きなbufferをコピーして小さくする方式ではなく、同じGPU bufferのaligned windowをbindingする。

- 🟢 [Done] source／destination window、task／workgroup分割、相対索引、索引の一括送信。
- 🟢 [Done] 境界・raw byte・差分membership・分割不能fallback、前後CPU計画と命令数の比較。
- ⭕️ [Pending] 実GPU readback・時間・電力・browser検証（Metal adapter NotFound）。

シンボリックリンクへのCargo path／import／include／実行時依存は追加していない。
[仕様と前後比較](partitioned-gather.md)。

## 分割gatherの計画・索引再利用

2026-10-17。Universal-3Dの形状bankとfull／dirty経路を再確認した。
ThreeXはInstance snapshotを複製せず、source slotの構成revisionから全体gather計画を再利用する。
相手ソースは参照のみで、cacheの形式・GPU索引はThreeXのRust Renderer内に閉じる。

- 🟢 [Done] 構成・容量・limitsによる全体計画cache、少数copy後のGPU索引再利用。
- 🟢 [Done] 部分gather後の索引再送、追加削除・形状変更の無効化、分割不能結果のcacheとcopy fallback。
- ⭕️ [Pending] GPU索引送信を省いた実GPU readback・性能・WASM／browser（Metal adapter NotFound）。

シンボリックリンクへのCargo path／import／include／実行時依存は追加していない。
[再利用契約と性能比較](gather-cache.md)。

## Plane／Sphereの任意分割

2026-10-17。Universal-3Dのbuild_geometry、sphereの緯度／経度と形状bankを参照した。
ThreeXは既存のXY Plane・unit radius=1 Sphere・外向きwindingを保持し、分割数をflagsで伝える。
相手のXZ Plane／radius=.5 Sphereや保存形式を直接流用しない。

- 🟢 [Done] bounded subdivisions、spec共有mesh、差分draw区間、u16／u32 index選択。
- 🟢 [Done] 分割変更でgather構成revisionを維持し、計画・GPU索引を再利用。保存v6とv1–5読込／復旧。
- 🟠 [Next] custom spec別batching・resource budget／generation handles。
- ⭕️ [Pending] 実GPU・WASM／browser、相手アプリとの文書交換。

シンボリックリンクへのimport／include／Cargo path／実行時依存は追加していない。
[仕様と検証境界](subdivisions.md)。

## Custom spec batching

2026-10-22。Universal-3Dのrenderer.rsにあるcube／sphere／plane別Batchと共有vertex bufferを再確認した。
ThreeXは3種類のGPU bankを維持し、その内部の索引をGeometrySpecごとにまとめる。
相手の固定形状batchを直接取り込まず、任意分割と既存の差分転送・gather cacheに合わせて実装した。

- 🟢 [Done] active specごとのdraw、移動slotの差分転送、索引変更に応じたcache無効化。
- 🟠 [Next] 構成編集時の再配置コスト削減、resource budget／generation handles。
- ⭕️ [Pending] 実GPU・WASM／browserの描画と性能。

シンボリックリンクへのimport／include／Cargo path／実行時依存は追加していない。
[計測と検証](spec-batching.md)。

## Custom specの局所再配置

Universal-3Dのrenderer.rsの固定3種類Batch・共有geometryを再確認した。
ThreeXは3 bankを保持したまま、任意specの連続groupを境界slotの交換で維持する。
相手の固定形状buffer実装やデータ形式は直接流用していない。

- 🟢 [Done] 全bank sortを除去し、追加・削除・spec交換の差分copyとgather cacheを維持。
- 🟠 [Next] resource budget／generation handles。
- ⭕️ [Pending] 実GPU／WASM表示・GPU性能。

シンボリックリンクへのimport／include／Cargo path／実行時依存は追加していない。
[局所再配置の比較・検証](local-spec-packing.md)。

## Custom mesh resource budget

Universal-3Dのrenderer.rsにある固定3種類Batchとgeometry cache、Instance差分更新を再確認した。
ThreeXは既定3形状を常駐させ、任意分割のcustom meshだけをbyte／件数budgetで管理する。
相手の固定geometry cacheを直接取り込まず、active specの保持と同kind既定meshへのfallbackを実装した。

- 🟢 [Done] 超過理由の照会、不要meshを新規生成前に解放、resident再利用・解放容量の再利用。
- 🟢 [Done] budget変更をScene／Instance bank／gather cacheから独立させる。
- 🟠 [Next] generation resource handles。
- ⭕️ [Pending] 実GPU解放・再生成・fallback描画、WASM／browser。

リンク先へのimport／include／Cargo path／実行時依存は追加していない。
[budgetの契約と検証](mesh-budget.md)。

## G01-A：generation付きmesh handle

Universal-3DのGpuSceneの固定3形状Batch・mesh map・差分Instance cacheの境界を再確認した。
ThreeXではcustom mesh mapに一時handleを保持し、pool epoch＋slot＋generationを検証してbufferを参照する。
共通poolはpending予約の取消と遅延／二重結果の拒否も提供する。

- 🟢 [Done] slot再利用・別pool／Renderer・epoch交換・generation枯渇・budget退避後の失効。
- 🟢 [Done] mesh budget・局所再配置・gather cacheを維持。
- 🟠 [Next] 永続asset ID／revisionとDocument・worker・保存形式の接続（G01-B）。
- ⭕️ [Pending] 実GPU上のhandle失効・再生成・複数surface表示。

シンボリックリンクへのbuild／実行時依存は追加していない。
[契約と検証](resource-handles.md)。

## G01-B：永続asset参照とworker

Universal-3Dのmesh::Source.revision・GpuScene.meshesを参照した。ThreeXはsource文字列と分け、
AssetId／整数revision／期待hashをDocumentの取引・history・checkpoint／journal v7へ保存する。

- 🟢 [Done] asset参照の復元、hash v3、永続receiptによるrestart再送、workerの有界ticket・guard接続。
- 🟢 [Done] 古い結果・Undo／Redo ABA・再起動・二重完了・hash不一致の拒否、差分Instance転送0。
- 🟠 [Next] G02：実asset page／粗いLOD／pool。
- ⭕️ [Pending] 実asset bytesの検算・GPU描画・WASM／他OS。

相手の型・保存形式へのimport／include／Cargo path／実行時依存は追加していない。
[仕様・検証境界](assets.md)。

## G02-A：asset pageと粗いLOD

Universal-3Dのmesh入力検証・MAX_TRIANGLES・共有BatchとSource.revisionを参照した。
ThreeXは全triangle常駐を採用せず、最大1MiBの検証付きpage・root／detail・固定容量poolへ分離した。

- 🟢 [Done] manifest／payload hash・seek/read、容量超過／失敗fallback、遅延結果拒否。
- 🟢 [Done] Rendererのproxy／root／detail経路、primitive除外、疎なasset layout stamp、既存gather維持。
- 🟠 [Next] G02-Bのnative host loader dispatch・完了配送・redraw。
- ⭕️ [Pending] 実GPUでのpage表示・readback・解放、WASM／browser。

相手の型・保存形式へのimport／include／Cargo path／実行時依存は追加していない。
[形式・API・制限・検証](asset-pages.md)。

## G02-B：native loader配送・surface／redraw

2026-10-25。Universal-3Dのmain.rsの非同期終了・lock順序とrenderer共有meshを参照した。
ThreeXはDocumentを複製せず、専用2 threadと8 leaseでreadを分離し、完了を順序付きworkerへ戻す。

- 🟢 [Done] root→detail配送、8MiB入力payload予約、I/O停止中の編集・終了drain。
- 🟢 [Done] Document／surface世代・段階の検証、古い結果拒否、target別on-demand redraw。
- 🟠 [Next] G03-A：CPU可視判定・LOD選択。
- ⭕️ [Pending] 実GPU／Wry・他OSでのupload・表示・終了。

相手の型・保存形式・import／Cargo path／実行時依存は追加していない。
[接続・上限・テスト](native-asset-loader.md)。

## G03-A CPU view selection (2026-10-25)

Universal-3Dのobject bounds・camera framing・shearの保守的stretch処理を読み取り参照。ThreeXは独立したclip-space AABB／SSEとinstance単位のhysteresisをRendererへ接続した。共有コードやsymlink依存は追加しない。[仕様と検証](cpu-lod.md)。

English: Independent CPU asset visibility/LOD selection preserves the page fallback and gather cache; Universal-3D remains a read-only architectural reference.

简体中文：独立实现CPU可见性／LOD选择，保留page fallback和gather缓存；Universal-3D仅作为只读架构参考，不添加符号链接依赖。

## MuTracerの設計参照

MuTracerは一時配置された読み取り専用の設計参考。コード・shader・テスト・生成物・assetを流用せず、削除後もビルド・実行・テストできる境界を維持する。[参考点と独立実装の方針](mutracer-reference.md)を記録した。Universal-3Dとの既存の協調方針とG03-Bの優先順位は維持する。

## G03-B1：独立した階層

Universal-3Dのworld bounds・shear処理を読み取り確認し、ThreeXにasset内coverage treeと独立したinstance空間階層を実装。[仕様・検証と残作業](hierarchies.md)。MuTracerからは責務分離の設計観点だけを参考にし、コード・shader・テストを流用していない。どちらの参照ディレクトリにも依存しない。

English: Independent asset/instance hierarchies preserve fallback and sparse GPU updates; no reference code or local-directory dependency is used.

简体中文：独立实现asset／instance层级，保留fallback和局部GPU更新，不复用参考项目代码，也不依赖参考目录。

## G03-B2a：親geometryの独立生成

Universal-3Dのworld bounds／shear契約を再確認。ThreeXに境界固定のendpoint縮約、元surface全体への対応からの誤差上限、元triangleを直接使う親生成を独立実装した。[仕様・検証](parent-simplification.md)。MuTracerのコード・shader・テスト・生成物を流用せず、両参照ディレクトリに依存しない。既存のcoverage・fallback・差分転送・gather索引を維持する。

English: Independent boundary-locked parent simplification preserves the Rust ownership and existing incremental renderer contracts. No reference code or directory dependency is used.

简体中文：独立实现固定边界的父级简化，保留Rust所有权、coverage和局部更新契约，不复用参考代码，也不依赖参考目录。
