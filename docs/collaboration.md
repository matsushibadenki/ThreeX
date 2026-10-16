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
- 🟠 [Next] 形状の交互配置でもdraw数を抑えるbatching。
- ⭕️ [Pending] 実GPU／browser表示・相手アプリとの文書交換。

相手ソースは参照のみ。シンボリックリンクへのimport／include／Cargo path／実行時依存は追加していない。
[仕様と検証境界](geometry.md)。
