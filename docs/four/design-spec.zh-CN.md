> 2026-10-15: [Content hash v1 implemented / 意味内容hash実装 / 内容 hash 已实现](content-hash.md)。

# 采用的 Four 整合规格

FX-01 至 FX-06 保留现有二进制命令与 JS 兼容接口。

- FX-01：Rust `Document` 拥有编辑历史和 Scene。渲染器及窗口通过 `scene()` 只读借用；仍支持独立 Scene。
- FX-02：`commit(expected_revision, commands)` 仅原子接受当前版本的编辑。Scene 验证循环、ID 与非有限值。失败不改变场景及历史。
- FX-03：撤销／重做产生新版本，恢复原实体 ID、属性及层级。新编辑清除重做；无变化及失败保留重做。
- FX-04：默认命令上限 256 KiB，历史上限 8 MiB，包含撤销及重做。单次正向与逆向命令合计超限时在修改前拒绝；容量不足时移除最旧撤销记录。这是 Rust 输入限制，并非已实现的 IPC 协议。
- FX-05：显式 `snapshot()` 复制 CPU 值，不受后续编辑影响。不得每帧调用或经 IPC 全量发送。渲染继续使用保留实例和版本差分。宿主翻译类型化错误。
- FX-06：Tauri 表面、停靠、GPU 能力与请求去重留待后续。v1 持久化已按 persistence.md 的范围实现。未实现 DocumentId、AssetId 及代际句柄。现有 u32 对象 ID 限于文档内。JS ID 不复用；撤销恢复原 ID。

不采用 Four D-01（不以兼容 API 为初期目标）。初期 Rust 所有者同时管理编辑语义与派生 Scene；独立 EvaluationWorld／GPU Scene 留待后续。v1 保存、历史恢复及检查点世代崩溃恢复已实现。语义哈希、拒绝过期异步求值任务及共享快照存储未实现。性能目标和跨引擎比较不是已取得的成果。浏览器／GPU 实机验证仍为 Pending。

2026-10-12 更新：已实现版本化检查点保存、历史恢复和世代回退，参见[持久化](persistence.md)。迁移、永久 DocumentId 和语义哈希留待后续。

2026-10-12: 已实现自动日志和未保存事务恢复。迁移及永久 ID 留待后续。 [Journal](autosave-journal.md)。

日志暂存采用增量 prepare/apply，不再复制整个文档。 [Performance](journal-staging-performance.md)。

显式提交组在一次同步后发布，并保留独立撤销边界。 [Group commit](group-commit-performance.md)。

分组前缀验证已改为变更 ID 覆盖层，保持原恢复语义。 [Performance](group-prefix-performance.md)。

结构验证和邻接表已改为仅更新受影响路径及边。 [Performance](structure-performance.md)。

2026-10-14：已实现 Rust worker、有界队列、时间／数量批处理和当前会话请求去重。Tauri／渲染器接线与永久请求／会话身份留待后续。 [Worker](worker.md)。

已实现永久文档身份、请求／会话 v1、检查点／日志 v2 迁移及重启后请求去重。Tauri IPC 留待后续。 [Protocol](request-protocol.md)。

协议 Commit 分组与 worker 批处理共用一次同步，保留独立结果／撤销和持久化正常前缀恢复。[性能与限制](protocol-group-performance.md)。

worker 协议候选保留已验证差分并直接写入已准备的分组，保持重启去重及恢复。[CPU 与 worker 测量](worker-prefix-performance.md)。

裸 Commit 的 worker 候选也采用差分准备并直接写入已准备分组，保留当前 worker 重发缓存及恢复边界。[性能与验证](bare-prefix-performance.md)。

已实现编辑内容 hash v1，支持 Document／快照、保存结果及 worker 显式查询，保留原有版本／请求校验。[规范与验证](content-hash.md)。

已实现会话生命周期，v3 检查点／日志持久化关闭记录并回收活动名额，兼容迁移 v1／v2，验证关闭及重启后的重发去重。[规范](session-lifecycle.md)。

已受理的编辑错误持久化为 v4 结果，重发保持原错误／版本，修正请求使用下一序号。[受理与恢复规范](durable-errors.md)。

2026-10-15：🟢 [Done] 已实现Tauri异步IPC、调用方授权和三语言错误DTO。实际invoke测试验证Rust worker所有权及重启后去重。实际Wry及渲染器集成仍待验证。 [IPC](tauri-ipc.md)。

2026-10-15：🟢 [Done] 已实现Rust渲染器连接和原生表面生命周期，借用worker的Scene，验证代际替换、暂停、销毁及有界重试。保持Document单一所有权和重启后请求去重。实际系统与GPU显示仍待验证。 [Native Renderer](native-renderer.md)。

2026-10-15：🟢 [Done] 已实现原生窗口回调连接和按需重绘调度器。持久Scene变更通知绘制，验证事件合并、队列繁忙时保留请求、有界重试、暂停、关闭及重启后去重。实际系统事件配送和GPU显示仍待验证。 [Redraw](native-redraw.md)。
