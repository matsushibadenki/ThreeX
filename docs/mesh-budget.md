# Custom mesh resource budget

🟢 [Done] custom GeometrySpecの共有meshを、GPU bufferの合計bytesとmesh数で制限する。

```rust
use threex_renderer::mesh_resources::MeshBudget;
renderer.set_mesh_budget(MeshBudget {
    max_bytes: 64 * 1024 * 1024,
    max_meshes: 128,
});
// 次の非zeroサイズのrender後に反映済み状態を参照する。
let status = renderer.mesh_budget_status();
// resident_bytes / resident_meshes / fallback: Vec<(GeometrySpec, MeshLimit)>
```

既定は64MiB／128 mesh。既定Box／Plane／Sphereは常駐し、このcustom budgetの対象外。
countはInstance数ではなく異なるcustom spec数。同じspecを何個表示しても1 meshとして数える。
bytesはvertex／index bufferのサイズの合計で、4-byte paddingを含む。
65,536頂点以下はUint16、それを超える場合はUint32 indexの実際のサイズを計上する。
生成前にサイズを算出し、deviceの`max_buffer_size`も各bufferに適用する。

## 上限を超えたとき

- `MeshLimit::DeviceBuffer`：vertexまたはindex bufferがdevice上限を超える。
- `MeshLimit::Count`：custom mesh件数上限。
- `MeshLimit::Bytes`：残りbyte容量が足りない。
- `MeshLimit::Handles`：admission後のhandle予約時にslot容量が尽きた。mesh生成前に拒否する。

判定順は上記の順。拒否したspecはCPU mesh生成・GPU buffer割り当てを行わない。
そのInstanceは同じkindの既定meshで描画する。モデル行列・RGB・Instance tag・Documentの分割指定は変えない。
完全な省略やDocument編集の拒否は行わず、状態照会でfallbackのspecと理由を確認できる。
budgetを0にすれば全custom specが既定meshへfallbackする。

現在参照されているresidentを優先し、既存resident同士／新規spec同士はGeometrySpec順で判定する。
大きいspecを拒否しても後続の小さいspecの判定を続ける。
不要meshと、新budgetに収まらなくなったmeshをすべてdropしてから、新規meshを生成する。
参照中で収まるmeshは同じリソースを再利用し、解放した容量はその更新内で別specに使用できる。
保持を外れたspecが再び必要になった場合は新しく生成する。未使用meshの常駐cache／buffer poolは設けない。

Scene更新またはbudget変更後の非zeroサイズrenderで再判定する。
非表示／削除でspec参照がなくなった場合にも解放し、空いた容量でそれまでfallbackだったspecを生成できる。
Sceneが変わらなくてもbudget変更は反映する。budgetだけの変更でScene stampを無効化せず、
Instance bank再配置・Instance再送・gather計画／GPU索引の再生成を要求しない。
set_mesh_budget自体は描画を実行しないため、利用側は通常のredraw経路でrenderを呼ぶ。

この上限はRendererが保持するcustom mesh bufferの論理的なサイズを制限する。
Instance／gather／depth／常駐既定mesh・CPU生成scratch・driver管理領域は含めない。
wgpuは実行中のsubmissionが参照するbufferの実解放を遅延できるため、物理VRAMの瞬間的な使用量の保証ではない。
RustのDocument所有、保存v7／hash v3／protocol v1を維持する。

## 検証

- 🟢 [Done] workspace debug 158件：core 54、renderer 26、storage 61、Tauri host 17。
- 🟢 [Done] renderer release 26件、JS 13件、全target／feature Clippy、format、diff whitespace検査。
- 🟢 [Done] Plane／Sphereの実meshから計上bytesを照合、255×255／256×256のindex幅境界。
- 🟢 [Done] byte上限ちょうど／1 byte超過、件数超過、device buffer上限、重複specと既定specの除外。
- 🟢 [Done] 大きなspecの拒否後も小さなspecを受け付け、拒否時にはfactoryを呼ばない。
- 🟢 [Done] mock resourceのDrop／identityで、resident再利用・生成前解放・0への縮小・容量再利用・空Scene・cache破棄を検証。
- 🟢 [Done] budget 0→1→2→1→0→2でgather full計画の索引再送なし、RGB差分1 slot・revision維持、構成変更時のcache更新。
- 🟢 [Done] 既存の局所再配置・分割gather／fallback・byte単位の転送照合、journal復旧・再起動後の重複排除。
- ⭕️ [Pending] native GPU testにbudget 0／既定budgetへの復帰・同じSceneでのfallback描画を追加したが、Metal adapter NotFound。実GPU解放・再生成・表示、WASM／browserは未検証。
- 🟢 [Done] custom meshの[generation resource handles](resource-handles.md)。
- 🟢 [Done] [永続asset参照のDocument接続](assets.md)。全GPUリソース共通のbudgetはG04の対象。

English: Custom meshes have a configurable 64 MiB / 128-mesh default budget, checked before generation.
Over-budget specs render with their kind's default mesh and expose a typed reason. Active residents are reused;
evictions precede allocations and freed capacity is reusable. Budget-only changes preserve Instance/gather state.
Tests pass on CPU; actual GPU release/fallback rendering remains pending. No Universal-3D symlink dependency was added.

简体中文：自定义mesh默认限制为64 MiB／128个，可在Rust侧配置，生成前检查。超限spec使用同类型默认mesh绘制，
并提供明确原因。复用活跃资源，先释放再分配，释放的容量可重新使用。仅修改budget不会改变Instance／gather状态。
CPU测试通过；实际GPU释放、重新分配及回退显示待验证。未添加对Universal-3D符号链接的依赖。
