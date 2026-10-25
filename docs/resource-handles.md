# G01-A：generation付きresource handle

🟢 [Done] custom mesh cacheを世代付きhandle経由へ接続した。
🟢 [Done] [G01-B](assets.md)の永続asset参照・Document／worker／保存接続。
この文書はG01-Aの契約を記録する。巨大geometry streamingはG02以降。

## 実装とAPI

`resource_handles::ResourcePool<T>`は固定slot上限を持つRust所有のpool。
`ResourceHandle`はprivateなpool epoch・slot・generationを持つ不透明な値で、Copy／比較／Hashが可能。
別poolやRenderer再作成後のhandleは、slotとgenerationが同じでもepochで拒否する。
process内でepochを重複させず、slot generationを増やす際もwrapさせない。
generationがu64上限に達したslotは廃止し、そのepochでは再利用しない。
epoch ID自体が枯渇する場合は新規epoch生成をfail-stopし、古いIDへwrapしない。

```rust
use threex_renderer::resource_handles::ResourcePool;
let mut pool = ResourcePool::new(64);
let ticket = pool.reserve().expect("capacity");
assert!(pool.is_pending(ticket));
pool.publish(ticket, "resource").unwrap();
assert_eq!(pool.get(ticket), Some(&"resource"));
assert!(pool.release(ticket));
assert!(pool.get(ticket).is_none());
assert_eq!(pool.publish(ticket, "late result"), Err("late result"));
```

- `reserve`：生成・job起動前にslotを予約する。pendingも容量を消費し、上限時はNone。
- `publish`：現行pendingへ1度だけ公開する。取消済み・別epoch・二重公開はErr(value)で結果を返す。
- `release`：pendingを取消す、またはready resourceをdropし、generationを進める。古いhandleにはfalse。
- `get`：現行ready resourceだけ参照できる。pending／廃止済み／古いhandleにはNone。
- `clear`：epochを更新して全job／handleを無効化し、resourceを解放する。

poolはjobを起動せず、mutationの同期は所有workerなどの利用側が行う。
`publish`で返された不要な値のdrop・job取消要求も利用側の責務。
このticketだけではasset内容revisionを自動検証しない。G01-Bのworker guardがasset参照とDocumentの変更を照合する。

## custom mesh・budget・gatherとの接続

spec mapはGPU buffer本体の代わりにhandleを保持し、draw時のmesh参照もpoolの検証を通る。
同じresident specはhandleを維持する。退避後の再生成には新しいgenerationを割り当てる。
budgetで解放する際はpool経由でresourceをdropしてから新規予約・mesh生成を行う。
slot予約不可なら`MeshLimit::Handles`としてfallbackし、CPU mesh生成やGPU割り当てを行わない。
既存のbyte／count／device buffer上限と同kind既定meshへのfallbackは維持する。

Rendererは次のRust APIを提供する。

```rust
let handle = renderer.custom_mesh_handle(spec);
if let Some(handle) = handle {
    let resident = renderer.is_custom_mesh_resident(handle);
}
```

handleはRendererが保持するcustom cacheに限定した一時IDである。
既定meshにはこのcustom handleを割り当てない。
device lossからRendererを再作成した場合は新しいpool epochを使う。
公開device fieldだけを差し替えてRenderer内部bufferを流用する設計にはしない。

Instanceの80-byte形式、GeometrySpec tag、bankのsource→destination対応、gather revisionは変更しない。
budgetだけの変更によるgather再生成も追加しない。
現行はcheckpoint v7／hash v3／request protocol v1。handleは引き続き保存・IPC receiptへ入れない。
この値は永続asset ID、process間ID、認可tokenではない。
slot metadataは過去の最大利用数に応じて保持し、mesh byte budgetの対象外。
全GPU／CPU metadataの共通budgetとsubmission中の物理解放管理はG04の対象である。

## 回帰・失効テスト

- 🟢 [Done] 同slot再利用後の古いread／release／publishを拒否。
- 🟢 [Done] pending予約の上限、取消、遅延結果・二重publish拒否、ready値の非上書き。
- 🟢 [Done] 別pool、clear後、pool破棄／再作成後のepoch分離。
- 🟢 [Done] generation上限slotの廃止、slot容量0・上限、clearによる新epochでの再利用。
- 🟢 [Done] release／clear／pool drop／返されたstale結果のDrop回数を照合。
- 🟢 [Done] meshの同一handle維持、budget退避・再生成による失効、別cache拒否、slot枯渇時の生成前fallback。
- 🟢 [Done] 既存の1 slot差分、idle、局所再配置、gather plan／GPU索引再利用とbudget縮小・再拡大。
- 🟢 [Done] workspace debug 164件（core 54、renderer 32、storage 61、Tauri host 17）、renderer release 32件。
- 🟢 [Done] JS 13件、全target／feature Clippy、fmt、diff whitespace検査。
- ⭕️ [Pending] native GPU testをhandle失効・再生成の検証へ拡張したがMetal adapter NotFound。実GPU表示／解放・複数surface、WASM／browserは未検証。

既存のUndo／Redo・保存／再起動・request再送・session終了・障害復旧テストは通過している。
G01-Bでは新しいasset参照の移行・hash・復旧を別途検証した（[結果](assets.md)）。

## CPU性能比較

変更前はspec mapからresourceを直接参照、変更後はmapからhandleを取得しpoolを検証する。
同じrelease設定で各3回、他のbuild・test・benchmarkと同時に走らせず順番に測定した。
10万Instance・4 custom specを用い、以下を各1万回測る。

- RGB：1 Instance変更→bank差分sync→mesh residency sync→4 mesh参照。
- idle：residency変更なしで4 meshを参照。全frameやschedulerの計測ではない。
- residency：budget件数0／4を交互に変更→residency sync→4 mesh参照。

resource本体はGeometrySpecのダミー値で、GPU mesh生成・I/O・描画は含めない。

```sh
cargo test -p threex-renderer --release mesh_handle_benchmark -- --ignored --nocapture
```

3回の中央値、各1万回の合計時間：

| 操作 | 直接参照 | handle検証付き |
| --- | ---: | ---: |
| RGB差分 | 2.046 ms | 2.390 ms |
| idle参照 | 0.097 ms | 0.120 ms |
| residency変更 | 1.770 ms | 1.925 ms |

[全sample](benchmarks/resource-handles.csv)。失効検証により追加コストがある。
RGBケースでは約17%増だが、1回当たりの差は約0.034µsという短い計測であり、
実frame時間やGPU性能への換算は行わない。転送slot数とgather cache契約は既存テストで維持を確認した。

## 次の実装境界

🟢 [Done] G01-Bの永続asset参照・保存v7／hash v3・worker guard・回帰テスト。
🟢 [Done] [G02-A](asset-pages.md)の粗いLOD・固定容量page poolとRenderer API。
🟢 [Done] [G02-B](native-asset-loader.md)のnative host loader dispatch。
🟠 [Next] G03-AのCPU可視判定・LOD選択。
Universal-3Dの固定Batch／mesh mapとの責務差を比較したが、シンボリックリンク依存は追加していない。

English: G01-A adds bounded generational resource pools and connects custom mesh residency to checked handles.
Pool epochs reject foreign/recreated-renderer handles; cancellation, slot reuse, delayed results and duplicate
publication cannot overwrite current resources. CPU tests pass with measured lookup overhead. Persistent asset references and Document/worker guards are now implemented in G01-B; actual GPU validation is Pending.

简体中文：G01-A实现有界generation资源池，并让custom mesh通过校验句柄访问。pool epoch隔离不同pool和重建的Renderer，
取消、slot复用、延迟结果及重复发布不会覆盖当前资源。CPU测试通过，计时记录了额外校验开销。
G01-B已实现持久asset引用、Document／worker guard及保存迁移；实际GPU验证保持Pending。
