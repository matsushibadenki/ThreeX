# G03-B2b2：多段pageのnative配送・描画接続

🟢 [Done] THREEXA2を既存の有界native loader、PagePool、Rust Rendererへ接続。v1／v2を共通の検証済みpage経路で処理する。

🟢 [Done] G03-B2b3：[有界background builderと生成結果cache](background-hierarchy-builder.md)。

🟠 [Next] view／cut最適化。view全体の階層訪問budget、細かいLODのhysteresis／cut cache、view依存の読み込み優先度は後続作業。

## 読み込みと所有

既存のtrusted Rust `LoadAsset`／`IpcHost::load_asset`を使う。`detail=false`はrootのみ。`detail=true`はv1なら従来どおりroot→detail、v2ならrootから幅優先にnodeを配送する。Document所有threadでI/Oやpage hash計算を行わず、2本のloader threadと最大8 leaseを維持する。1 leaseは同時に1 pageだけを処理し、入力を消費した後に次のpageを要求する。

幅優先順は、親の次に兄弟pageを揃え、片側の深い枝だけで容量を使い切ることを避ける。容量超過・読込／検証失敗・surface失効でそのleaseの続行を止め、既に正常な親とpageを維持する。未読の別枝へ自動retryする機構や自動LRUはまだない。全nodeを読み切る保証はなく、常駐上限内で完了するところまで配送する。

初回にAssetRefに対してmetadataを認証し、同一leaseの続行taskへ共有する。metadataはArcで保持し、pageごとにheaderや階層を再読込・再解析しない。続行ではファイル長と各pageのhash・bounds・indexを検証する。別lease／別要求では初回の認証をやり直し、pathだけをkeyとするglobal cacheは導入しない。

payload、metadata、job guardはRust内部に保持し、Windowには小さい命令・結果だけを返す。Document／surface generationの照合は各完了時に行い、UndoによるABA・再起動・windowの世代変更で古い結果を公開しない。request再送の重複排除や保存形式は変更しない。

## 容量

v1／v2は同一PagePoolのbytes／slot quotaを共有する。v2用に別枠のGPU容量を増設しない。rootはasset解放まで保持し、`evict_asset_detail`は全非root pageとその予約を解放する。`release_asset`はrootも解放する。manifest登録はRendererあたり最大128、v2 metadataはassetあたり最大4096 node。

loaderの`reserved_payload_bytes`はlease数×1 MiB、`reserved_manifest_bytes`はlease数×360,520 bytesのencoded metadata枠を示す。後者はRustのdecoded treeやallocator overheadまで含む実測RAM量ではない。decoded metadata、幅優先queue、Rendererのmanifest／選択cacheもnode・asset・lease数の上限で制限される。同じmetadataのcloneは内部配列を共有する。

## 描画とfallback

Rust RendererはCPUのinstance空間階層とroot SSE／hysteresis選択を維持する。v2ではinstanceごとにresident-onlyの階層cutを作り、子がすべて常駐して予算内にあるときだけ親を置き換える。さらに深いnodeにはそのbounds・errorと同じview-projection／modelからSSEを適用する。root未到着時は既存のbounds proxyを使う。分岐の一部だけを単一page APIで返して穴を作らない。

`HierarchyCutBudget`はinstanceごとの訪問数64・draw数32がdefault。`Renderer::set_hierarchy_cut_budget`は正の値かつ4096以下を受け付ける。cut APIへ0を直接渡した場合はroot-onlyとして扱う。これはview全体の上限ではなく、instance数に比例する仕事は残る。rootのhysteresisは既存どおりで、深いnodeは現在のSSE thresholdを直接使う。詳細levelごとの履歴やcut cacheは未実装。

v2の複数geometryは同じsource Instance bufferから描画する。可視cutの変更やpage到着によるInstanceの再packing・全量転送・gather索引再送は行わない。v1の連続instance draw rangeは従来の経路を維持する。v2はCPU基準版としてinstanceごとにcut配列とdrawを作り、同一cutのinstance batch化はまだ行わない。

## Rust APIと互換性

`AssetManifest::encoded_len/decode`と`AssetReader`がmagicを見てv1／v2を判別する。v1 encoderのbytes、THREEXA2、checkpoint／journal v7、IPC protocol v1は変更しない。v2専用の`HierarchyAssetReader`も利用できる。

page/node IDを255で切り詰めないため、Rustのpage引数・`PageRequest::level`・`ValidatedPage::level`・`SurfaceBackend::upload_asset`等のlevel型をu8からu32へ拡張した。独自backendやu8変数を渡すRust呼び出し元は型を更新する必要がある。JSONコマンドへの大容量dataや新しい権限は追加しない。

## 検証

- 3段7nodeの実ファイルをworkerで幅優先配送し、`[0] → [1,4] → [2,3,4] → [2,3,5,6]`とcoverageを維持すること。
- root以降のmetadata再利用、1 lease内の7 page処理と容量解放。
- v2のUndo ABAで旧rootを拒否し、保存・再起動後に現在の参照を読み込めること。
- 容量0・root分しか入らない容量・子page破損でproxy／rootを維持すること。
- 300 nodeのmetadataとnode ID 299を保持し、manifest cloneが内部階層を共有すること。
- 部分常駐・訪問／draw budget・全非root退避・stale completion時の予約解放。
- 1200 instanceの交互配置でpage状態変更後もInstance転送0、bank revision不変、gather索引再送なし。
- 既存のv1、native scheduler、保存・再送重複排除・回復テスト。

検証結果：Rust workspaceはdebug／releaseとも214 passed（core 75、renderer 44、storage 77、Tauri host 18）。JS 13 passed、Clippy all-targets／all-features、format、diff checkも通過。

⭕️ [Pending] 実GPUの描画・亀裂／popping・GPU性能。native GPU pipeline testにはv2のpage到着・root-only budget・退避後のrenderを追加したが、Metal adapter NotFoundで開始できなかった。CPUのcut検証やWGSL検証を実画像の検証とは扱わない。

Universal-3Dのworld bounds／shearとRust所有境界を維持した。MuTracerのコード・shader・テストは流用せず、両参照ディレクトリへの依存を追加していない。

English: THREEXA2 now travels through the bounded native loader and Rust GPU page path. Breadth-first delivery reuses authenticated metadata within a lease. Atomic resident cuts preserve parent/proxy fallback and existing sparse/gather updates. Node IDs are u32. Actual GPU validation remains Pending; bounded background generation and its session cache are implemented.

简体中文：THREEXA2已连接有界native loader与Rust GPU page路径。广度优先配送在lease内复用已认证metadata，原子resident cut保留父级／proxy fallback及局部更新和gather缓存。node ID扩展为u32。实机GPU验证保持Pending，已实现有界后台构建与session生成结果缓存。
