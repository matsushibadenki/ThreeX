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
