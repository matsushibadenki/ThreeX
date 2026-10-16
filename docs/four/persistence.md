> 2026-10-16：geometry対応により現行保存形式はcheckpoint／journal v5、content hash v2。旧v1–4を移行。[仕様](../geometry.md)。

> 現行checkpoint／journalはv4、受理済み編集エラーを保存する。[最新仕様](durable-errors.md)。

> 現行checkpoint／journalはv3。v1／v2読込とsession終了記録の移行を実装。[最新仕様](session-lifecycle.md)。

> 意味内容hash v1を実装。checkpoint format v2は維持し、保存receiptと明示queryにhashを返す。[最新仕様](content-hash.md)。

> 保存形式の現行版はcheckpoint／journal v2です。[永続IDと再送protocol](request-protocol.md)を参照。以下のv1仕様は導入時の記録です。

> この世代保存レポートの後、未保存変更のjournal復旧を実装。最新の仕様・制約は [autosave-journal.md](autosave-journal.md) を参照。

# バージョン付きDocument保存と復旧

2026-10-12 JST。Rust APIで保存・読込を実装。WebViewへ文書全量やGPUデータを転送しない。
`threex-core`はOS非依存のcodec、`threex-storage`はネイティブfilesystemホストを担当する。
Universal-3Dのversion検証・一時保存・保存中のdirty維持を参照し、独立実装した。
相手のソースを変更せず、シンボリックリンク・外部パスへの依存も追加していない。

## APIと所有者

```rust
use threex_core::Document;
use threex_storage::{DocumentStore, PreparedSave};
let document = Document::default();
let store = DocumentStore::new("project/document-store"); // 1文書専用ディレクトリ
let pending = PreparedSave::capture(&document)?;
let receipt = store.save_prepared(pending)?;
assert!(receipt.is_current(&document));
let loaded = store.load()?;
let restored = loaded.document;
```

コードはAPI説明の抜粋（`?`には適切なResultを返すホスト関数が必要）。
`save(&Document)`は同期呼出し。背景保存はRust側で`PreparedSave`を取得後、IO workerへ渡す。
receiptは取得時のSceneStampを保持するため、保存中の編集・Undo・別Documentはdirtyのまま。
最新の正常世代より古いrevisionを保存する要求は`StaleSave`で拒否する。
読み込みは新しいDocumentを返し、成功確認後にホストが差し替える。失敗時に現在の文書を破壊しない。

## v1形式

Little-endian binary、magic `THREEXD\0`、major version u32=1。
以降u64でrevision、history budget、command budget、object count、undo count、redo count。
ObjectはID順の64byte属性record（opcode=2）で、既存position／rotation／scale／color／flags／parentを保存。
各履歴はforward長・inverse長（u64）とバイナリコマンド。末尾CRC32は全preceding bytesを検査。
CRC32は偶発破損の検出用で、認証・改ざん防止ではない。

読込上限128 MiB、object上限500k、history上限8 MiB、command上限256 KiB。
保存も同じ上限。独自設定が上限を超えるDocumentは明示エラーになる。
長さ・件数・ID順・重複・NaN・親参照・循環・フラグ・履歴budgetを検証し、Undo/Redoを
隔離Sceneで往復して現内容に戻ることを確認してから受け入れる。末尾余剰bytesも拒否。
未知versionは読込／保存を拒否し、古い形式へ暗黙downgradeしない。移行処理は未実装。
GPUバッファ位置・SceneStamp・OSハンドルは保存せず再生成。Undo/Redoと元ID、revisionは復元。
DocumentId／AssetId、意味内容hashは次工程。保存先は文書専用とし、異なるDocumentを同じ保存先へ混ぜない。

## commitと復旧

1. OS advisory lockで同じ保存先のwriterを直列化。競合はBusy。プロセス終了時にOSがlockを解放する。
2. `generation-<20桁番号>.tmp`へ書き込み、ファイルを`sync_all`する。
3. 同一ディレクトリ内で新しい不変`generation-<番号>.txd`へrename。
4. macOS／Unixでディレクトリをsync。成功後、新世代と直前の正常世代を保持し、古い世代をbest-effortで削除。

loaderは完成世代だけを番号降順で検査し、破損した最新世代なら直前の正常世代へ戻る。
戻った場合`recovered=true`と採用世代を返す。全世代破損ならNoValidGeneration。
途中のtmpは読まない。保存時に同番号の放棄tmpを置き換える。
正常な一回の保存後は基本2世代。繰り返す中断や削除失敗では残存世代が増えるため、4096世代で明示拒否。
IO権限エラー等は破損扱いで隠さず返す。未知versionも自動rollbackせず明示拒否する。

rename後のエラーは「保存されなかった」と断定できない。`load()`して実際の世代を確認する。
loaderはtmpを昇格しないため、切替前の未保存編集は失われる。編集中の自動journal／autosaveは未実装。
同期済checkpointまでの復旧であり、未保存変更まで復元する保証はない。

## テストの証跡

- version付きroundtrip：全属性、ID、revision、Undo/Redo、Scene identity再生成。
- 全切断位置、checksum破損、未知major、異常件数、checksumを合わせたNaN／cycle／不正履歴拒否。
- 最新世代破損から正常世代へ復旧、全世代破損、放棄tmp、保存先lock競合。
- 5段階でIO失敗を注入（部分write、全write、file sync、rename、directory sync）。
- 同じ5段階で子プロセスをexitし、Drop未実行でも旧／新の完全世代を復元・再保存。
- 保存中編集のdirty保持、古い背景保存拒否、成功後2世代保持。

```sh
cargo test --offline --workspace
cargo test --offline --release -p threex-core -p threex-storage
cargo clippy --offline --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
node --test tests/compat/scene.test.js
```

macOS開発環境で確認。実際の電源断、物理disk不足、filesystem／hardwareのdurabilityは未検証。
IO failure注入は物理disk-full試験とは区別する。Windowsのdirectory syncは未実装で
Unsupportedを返す（rename済の可能性あり）。Windows／Linux実機、network filesystemはPending。
Tauriのファイル認可・UI・未保存確認・エラー翻訳は次工程。

- 🟢 [Done] Document v1保存・読込、履歴復元、世代rollbackと障害注入／プロセス終了テスト。
- 🟢 [Done] 永続Document ID、意味内容hash、autosave／journal・移行・Tauri接続（[仕様](tauri-ipc.md)）。
- ⭕️ [Pending] 電源断・実容量不足・Windows／Linux durabilityと実UI確認。

English: Version 1 stores stable IDs, complete attributes, revision, and bounded undo/redo.
Immutable generations retain a previous valid checkpoint; corrupt latest data falls back explicitly.
Prepared saves preserve dirty edits and reject older revisions. Filesystem IO is isolated from core.
Unsaved changes, autosave journaling, migrations, permanent Document IDs, and Tauri wiring are not implemented.
Actual power loss, full disks and cross-platform durability remain Pending.

简体中文：v1 保存稳定 ID、完整属性、版本和有界撤销／重做历史。不可变世代保留上一份正常检查点，
最新损坏时明确回退。预备保存保持后续编辑的 dirty 状态，拒绝较旧版本。文件 IO 与核心分离。
未保存修改、自动保存 journal、迁移、永久 Document ID 及 Tauri 接线未实现。
实际断电、磁盘满和跨平台持久性仍为 Pending。
