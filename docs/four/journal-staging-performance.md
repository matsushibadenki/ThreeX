> 後続で明示group commitを実装。[最新の同期回数・性能・復旧境界](group-commit-performance.md)を参照。

# Journal差分トランザクションと性能比較

2026-10-12 JST。journal追記前に全Documentをcheckpointへ変換し復元する経路を廃止。
`Document::prepare_commit/prepare_undo/prepare_redo`は変更IDの最終値と必要なinverseだけを準備する。
Sceneのコマンド・親参照・循環検証を準備段階で一度行い、`apply_prepared`は検証済み差分を適用する。
通常のDocument commit／Undo／Redoも同じ経路を利用し、別の検証実装を増やさない。

PreparedTransactionは内部状態を非公開とし、元Sceneのidentityとrevisionに結び付く。
準備だけではScene、revision、履歴、stats、GPU差分journalを変更しない。
古いrevision／別Documentへの適用は変更前に拒否する。
AutosaveDocumentはwriterを所有し、準備→record追記→sync→同じDocumentへ適用の順序を守る。
同期エラー時は差分を破棄し、本Documentを変更しない。既存のretry・末尾復旧契約を維持する。

## 計測条件

macOS arm64、rustc 1.99.0、release thin LTO、offline。100／1k／10k／100kの独立Mesh。
初期Sceneは履歴なし、revision=1のcheckpointから作成。各回末尾IDのposition.xを変更する。
4回warmup、24回測定、前後2組。初期構築・コマンド生成・保存先作成は時間外。

- `stage_apply`：旧checkpoint encode/decode＋候補commit＋本commitと、新prepare＋applyを比較。IOなし。
- `journal_sync`：実AutosaveDocument.commit全体。record writeとsyncを含む。

GPU時間・実表示遅延・IPC・電源断durabilityの測定ではない。
24サンプルではp99がmaxと同じで、尾部精度は限定的。極小CPU時間は時計分解能・揺らぎの影響が大きい。
初期履歴なしなので、大量の既存履歴や複雑な親子構造について一般的な倍率を主張しない。

## 最初の前後組（p95 ms）

| Objects | CPU準備＋適用 前 → 後 | journal同期込み 前 → 後 |
| ---: | ---: | ---: |
| 100 | 0.182917 → 0.001542 | 7.465333 → 4.546917 |
| 1k | 1.540375 → 0.000500 | 7.216208 → 4.439208 |
| 10k | 16.274625 → 0.000584 | 26.176375 → 4.613958 |
| 100k | 158.873584 → 0.001416 | 156.686750 → 4.488208 |

2組目の100k p95：CPU161.721542→0.000875 ms、同期込み179.317041→4.515459 ms。
同期込みでは改善後も約4.5 msが残る。小規模ではfsyncの揺らぎが支配的で、CPUの倍率を
同期込みの効果として扱わない。両操作は別サンプルなので、CPU列が同期込みより大きい場合もある。
raw CSVにmedian／p95／p99／maxと全サイズを保存。RSSは未計測。

## 制約と次工程

Transform／color等の準備量は変更対象数、適用量は変更と影響する子孫に従う。
Entity追加削除／reparentでは従来の全体階層検証・隣接表再構築が残る。
履歴のredo破棄・古いundoの移動にも履歴件数に応じた処理がある。
checkpoint保存・圧縮・初期読込は引き続き全Documentを扱う。
各編集でのfsyncは変更していない。group commit／worker／Tauri接続／request重複排除は次工程。

## 検証と再現

core25件、storage13件、WGSL1件、JS4件が成功。core／storageはdebugとreleaseで確認。
準備の不変性、stale／異なるidentity拒否、Undo/Redo、親子構造の無効batchを検証。
既存のappend失敗・retry・末尾全切断／破損・compaction失敗・process exit試験も成功。

```sh
cargo run --offline --release -p threex-storage --example journal_bench --features prepared
cargo test --offline --workspace
cargo test --offline --release -p threex-core -p threex-storage
cargo clippy --offline --workspace --all-targets --all-features -- -D warnings
```

ベンチの`prepared` featureは測定するCPU経路の選択のみ。production journalは常に差分経路。
変更前core／storageと同じベンチを`../benchmarks/journal-staging-baseline.zip`に独立workspaceとして保存。
一時ディレクトリへ展開後、featureなしで`cargo run --offline --release -p threex-storage --example journal_bench`を実行する。
`journal-staging-{before,after,repeat-before,repeat-after}.csv`、comparison CSV、metadata JSONを同ディレクトリに保存。
外部Four／Universal-3Dへの依存はない。

- 🟢 [Done] 全Document複製を廃止、差分prepare/applyと前後2組のCPU・同期込み比較。
- 🟠 [Next] group commit／worker接続、構造変更の差分検証、性能試験の対象拡張。
- ⭕️ [Pending] 実電源断・OS別durability、Tauri UI接続、GPU実表示。

English: Prepared transactions validate only changed objects, preserve scene identity,
and publish after journal synchronization. Full-document staging is removed. Structural
hierarchy work and per-edit fsync remain. Measurements cover one independent transform
edit, not GPU rendering or every workload.

简体中文：预备事务仅暂存变更对象，保持场景身份，并在日志同步后发布。
已移除全量文档复制。结构层级扫描及每次编辑 fsync 仍存在。
测量针对单个独立变换编辑，并不代表 GPU 渲染或所有工作负载。
