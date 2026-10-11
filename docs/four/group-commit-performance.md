> 後続で累積prefixの検証を差分化。[最新の準備性能と制約](group-prefix-performance.md)を参照。

# Journal group commitと性能比較

2026-10-12 JST。`AutosaveDocument::commit_group(expected_revision, &[&[u8]])`が
複数の通常編集を個別recordとして追記し、一度のsync後に同じDocumentへ公開する。
単独commit／Undo／Redoは従来の同期方式を維持。UI／workerが明示的にまとめるAPIであり、
タイマーによる自動集約やTauri接続は未実装。

最大32編集、入力合計1 MiB、各編集は既存command／history上限。全編集を検証してからIOへ進む。
先行編集で作ったIDを後続編集が使える。各編集のrevisionとUndo境界を維持し、no-opはrevisionを
増やさずrecordも作らない。全件no-opならsyncなし。1件でも無効ならグループを追記しない。
PreparedGroupは開始SceneStampに結び付く不透明な値。中間のfuture planを別々に取り出せず、
古いrevisionや別Documentへ適用することを防ぐ。Document／Scene全量は複製しない。

## 公開と復旧の契約

成功時は全recordが同期済みで、編集を順番に適用してrevision列を返す。
同期前に失敗した場合、本Documentは変更しない。同じownerのretryは未確認suffixを置き換える。
クラッシュ後は最後まで書かれた正常なtransactionまで復旧する。グループ全体を
all-or-nothingにする形式ではなく、各transactionが復旧の境界。
partial write時はグループの先頭数件が残り得る。全write後に応答前で終了した場合は、
未応答のグループ全件が復旧される可能性もある。復旧revisionを読んで再同期し、
盲目的な再送はしない。request重複排除は次工程。

API抜粋（`?`はResultを返すRustホスト関数で利用）：

```rust
let revisions = owner.commit_group(owner.document().revision(), &[&first, &second])?;
```

write/write/syncの3段階でIO失敗と子プロセスexitを注入。公開前の不変性、retry重複なし、
OS lock解放と追記再開を確認。末尾を全byte位置で切断し、完全record数に一致するrevisionまで復旧。
グループ各recordの破損ではそれ以前まで復旧し、後続を飛び越えて採用しない。
依存編集、create→update→delete、no-op混在、個別Undo/Redo、上限拒否、1sync／0syncもテスト。

## 前後計測

macOS arm64、release thin LTO、offline。初期100／10kの独立Entity（Meshなし）へ新IDを追加する。
同じ入力の1／8／32編集を、旧版は逐次commit、新版はcommit_groupで実行。
4回warmup＋24回測定、前後2組。初期Scene作成・checkpoint保存・コマンド生成は時間外。
測定は検証、journal write／sync、Scene適用を含む。サンプルごとにEntity数がgroup_sizeずつ増える。
CSV objectsは初期数で、最終数ではない。GPU／IPC／実表示／実電源断性能を示すものではない。

最初の前後組、p95 ms：

| 初期Objects | 編集数 | 前 → 後 ms | sync回数／group 前 → 後 |
| ---: | ---: | ---: | ---: |
| 100 | 1 | 4.461500 → 4.283667 | 1 → 1 |
| 100 | 8 | 34.960625 → 4.478791 | 8 → 1 |
| 100 | 32 | 135.085500 → 5.958833 | 32 → 1 |
| 10k | 1 | 4.583375 → 5.315208 | 1 → 1 |
| 10k | 8 | 41.966959 → 10.537583 | 8 → 1 |
| 10k | 32 | 172.306084 → 37.196292 | 32 → 1 |

2組目10k／32は169.395625→35.680625 ms。小さなグループはfsyncの揺らぎと準備コストが支配的。
10kの1編集は最初の組で悪化しており、同期回数も減らない。単独操作の改善は主張しない。
最終計測の10k／32にはmax 73.416750 msの尾部遅延もあった。初期の試作計測より遅く、環境の揺らぎと構造更新コストを含む結果として記録する。
24サンプルではp99=maxとなり尾部精度は限定的。CSVにmedian／p95／p99／maxを保存。
fsync回数はコード経路に基づく予定回数で、テストhookでgroup非空1回／全no-op0回を確認。
OS syscall tracingや物理ストレージ書込回数は測っていない。

## 制約と次工程

グループ内の変更対象に対する累積prefixを検証するため、グループサイズに応じて準備処理が増える。
構造変更の全体階層走査・隣接表再構築も残る。group sizeに対して常に線形とは主張しない。
既存Undo／Redoを混ぜるgroup API、time-based worker batching、request重複排除は未実装。
各グループのfsync、checkpoint圧縮、その他の保存syncは維持。明示groupだけを減らす。
実電源断・実disk-full・Windows／Linux durability・実UI接続はPending。

検証：core25件＋storage16件＋WGSL1件＋JS4件成功。core／storageはdebug／releaseを確認。

```sh
cargo run --offline --release -p threex-storage --example group_bench --features group
cargo test --offline --workspace
cargo test --offline --release -p threex-core -p threex-storage
cargo clippy --offline --workspace --all-targets --all-features -- -D warnings
```

`group` featureはベンチのAPI選択だけを変える。production APIはfeatureなしで使える。
`../benchmarks/group-commit-baseline.zip`に旧core／storageと同じbenchを独立workspaceとして保存。
展開先でfeatureなしのgroup_benchを実行。raw `group-commit-{before,after,repeat-before,repeat-after}.csv`、
comparison CSV、metadata JSONも同ディレクトリに保存。外部symlink依存なし。

- 🟢 [Done] bounded group commit、sync回数削減、正常prefix復旧・前後性能比較。
- 🟠 [Next] workerの時間／件数batching、request重複排除、累積prefix準備の最適化。
- ⭕️ [Pending] 実電源断・他OS durability・Tauri UIの復旧。

English: Explicit groups retain separate revisions and undo boundaries, with one sync
before publication. A crash can recover only a complete prefix; the group is not an
atomic recovery unit. Timer batching, undo/redo groups and request deduplication remain future work.

简体中文：显式分组保留各编辑的版本与撤销边界，在一次同步后发布。崩溃可恢复完整前缀，
整个分组并非原子恢复单位。定时批处理、撤销重做分组及请求去重留待后续。
