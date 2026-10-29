# ThreeX implementation roadmap

- 🟢 [Done] Cargo workspace: core / renderer / wasm; JavaScript facade.
- 🟢 [Done] Scene hierarchy, XYZ transforms, inherited visibility, entity deletion.
- 🟢 [Done] Binary batched commands, complete validation before mutation.
- 🟢 [Done] wgpu cube pipeline, depth testing, shared geometry, instanced draw.
- 🟢 [Done] Retained GPU buffer capacity; unchanged scene skips instance upload.
- 🟢 [Done] On-demand example; English / 日本語 / 简体中文 text.
- 🟢 [Done] Rust and JS contract regression tests.
- 🟢 [Done] Transactional changed-entity validation; empty batch performs no scene work.
- 🟢 [Done] Coalesced dirty-range GPU writes, including buffer reallocation handling.
- 🟢 [Done] SoA transform/visibility/material arrays with private reusable slots.
- 🟢 [Done] Cached local/world transforms; affected subtree updates and parent-first iterative evaluation.
- 🟢 [Done] Retained packed instances, in-place transform/color edits, no-op batches skip work.
- 🟢 [Done] Iterative cycle validation and deep hierarchy regression tests.
- 🟢 [Done] SoA before/after CPU benchmark, startup/all-dynamic costs, process peak RSS.
- 🟢 [Done] custom meshのgeneration付きresource handle、pool epoch・pending予約・失効判定（resource-handles.md）。
- 🟢 [Done] Revision-aware GPU upload ranges; removed full Instance comparison/copy.
- 🟢 [Done] Bounded dirty journal, skipped-update merging, independent renderer stamps, safe full-upload fallback.
- 🟢 [Done] Upload-preparation before/after release benchmark and repeated all-dynamic comparisons.
- 🟢 [Done] Incremental membership packing: append/swap-remove, entity-index lookup, shrink-safe dirty ranges.
- 🟢 [Done] Visibility/Mesh/entity lifecycle before/after CPU and planned-upload benchmarks.
- 🟢 [Done] Incremental structural validation and adjacency for create/delete/reparent.
- 🟢 [Done] Shared material RGB changes and material replacement mark affected meshes dirty.
- 🟢 [Done] Perspective camera rotation／parent TRS／zoom／world lookAt、Rust view／projection math（camera.md）。
- 🟢 [Done] Plane／Sphere geometry、共有GPU mesh・差分描画区間、Box wire／Instance互換（geometry.md）。
- 🟢 [Done] geometry別GPU batching、最大3 draws、dirty slotのGPU内コピー・差分membership（geometry.md）。
- 🟢 [Done] 対応limits内の交互配置の初回／全更新をcompute gatherへ集約、少数差分copy・limits fallback維持（gather-performance.md）。
- 🟢 [Done] binding上限超過時のaligned window・task／dispatch分割gather、差分copy・分割不能fallback（partitioned-gather.md）。
- 🟢 [Done] 構成revisionに基づく分割gather計画・GPU索引再利用、部分gather後の再送・fallback維持（gather-cache.md）。
- 🟢 [Done] Plane／Sphereの任意分割（上限256）、shared mesh・spec差分draw・Uint32 index、保存v6／v1–5移行（subdivisions.md）。
- 🟢 [Done] custom分割spec別batching、移動slotの差分copy・gather索引cacheの再検証（spec-batching.md）。
- 🟢 [Done] custom spec構成変更を境界slotの局所交換へ置換、移動slotのみ転送、gather cache契約維持（local-spec-packing.md）。
- 🟢 [Done] custom meshのbyte／件数budget、同kind既定meshへのfallback・状態照会、解放・再利用とgather cache維持（mesh-budget.md）。
- 🟢 [Done] G01-B：永続asset ID／revision・期待hash、Document／worker、保存v7／hash v3、restart再送・古い結果拒否（assets.md）。
- 🟢 [Done] G02-A：最小asset page形式・root／detail LOD・固定容量pool、Rust Renderer接続（asset-pages.md）。
- 🟢 [Done] G02-B：native専用2 thread／8 lease loader、完了配送・surface世代guard・対象別redraw（native-asset-loader.md）。
- 🟢 [Done] G03-AのCPU bounds／frustum判定・LOD選択（[詳細](cpu-lod.md)）。
- 🟢 [Done] G03-B1のinstance空間階層・asset coverage階層・正確なcluster分割（[詳細](hierarchies.md)）。
- 🟢 [Done] G03-B2aの境界保持簡略化と保守的error評価（[詳細](parent-simplification.md)）。
- 🟠 [Next] G03-B2bの多段page接続とbackground builder。camera view offset／film設定などの互換API。
- ⭕️ [Pending] compute gatherの実GPU readback・性能・閾値調整。
- 🟢 [Done] Release CPU scene benchmark at 100 / 1k / 10k / 100k objects, before/after CSV.
- 🔴 [Later] GPU Scene／compute cullingと10万cube比較：巨大geometry計画G03／G04の後、G05で実装・比較。
- 🟢 [Done] 粗いasset LOD・固定容量page poolのRust API：巨大geometry計画G02-A。
- ⭕️ [Pending] G02の実GPU proxy／root／detail表示・readback。
- 🔴 [Later] 階層LOD／streaming／indirect draws／Hi-Zは巨大geometry計画G03〜G06で段階導入。Forward+／render graph／PBR。
- 🔴 [Later] glTF/GLB, Meshopt/KTX2, GPU animation and particles, streaming.
- 🔴 [Later] Worker/OffscreenCanvas, native/Tauri 2 docking and floating panels.
- ⭕️ [Pending] Browser WASM/WebGPU visual verification and Apple GPU profiling.

Historical CPU results are in benchmark-before.csv / benchmark-after.csv.
Current SoA measurements are in benchmarks/soa-_.csv and benchmarks/soa-memory-_.json.
Historical transactional-overlay results are in performance.md; current SoA results
and limitations are in soa-performance.md. GPU/power measurements remain pending.

## 巨大geometry：MacBook Airで数千億ポリゴンのデータを扱う計画

本節は設計・実装計画であり、数千億ポリゴン対応を実装済みとするものではない。
目標は「総データ量が増えても、常駐量と1フレームの処理量を予算内に保ち、操作を継続できること」。
全ポリゴンの同時常駐・全件更新・毎フレーム描画は目標にしない。
2,000億三角形は32bit indexを3つずつ持つだけで約2.4TB（decimal）になるため、
頂点・材質・階層を含む実データには外部ストレージなども必要になる。
反復Instanceから算出した見かけのポリゴン数と、固有geometryの実データ量は分けて報告する。

### 現在の土台と、着手順

- 🟢 [Done] Rust側のDocument／Scene／Renderer所有、差分Instance更新、spec別描画と局所再配置。
- 🟢 [Done] 分割gather、revisionに基づく計画・GPU索引cache、limits非対応時のcopy fallback。
- 🟢 [Done] custom meshの64MiB／128件budget、超過時の同kind既定mesh、解放・再利用（[mesh-budget.md](mesh-budget.md)）。
- 🟢 [Done] G01-A：custom meshと共通poolのgeneration付きhandle・失効・pending公開（[resource-handles.md](resource-handles.md)）。
- 🟢 [Done] G01-B：永続asset参照とDocument／workerの制御面、checkpoint／journal v7・hash v3（[assets.md](assets.md)）。
- ⭕️ [Pending] 実asset payload／GPUを伴う復元はG02以降で検証。
- 🟢 [Done] G02-A：最小asset形式、root／detail選択、固定容量page pool、seek/readとRenderer API（[asset-pages.md](asset-pages.md)）。
- 🟢 [Done] G02-B：native hostの有界loader dispatch・完了配送・対象別redraw（[仕様](native-asset-loader.md)）。
- 🟢 [Done] G03-A：CPU基準のbounds／frustum判定・screen-space errorによるLOD選択（[詳細](cpu-lod.md)）。
- 🔴 [Later] G03以降はG01／G02の回帰・容量試験を通してから順に進める。大規模GPU最適化を一括導入しない。
- ⭕️ [Pending] MacBook Air実機での描画・GPU時間・長時間負荷・実メモリ測定。

推奨実装順：G01 → G02 → G03 → G04 → G05 → G06 → G07。
G08の計測項目はG01から記録し、実機確認が可能になった段階で性能の合否を判定する。
既存のcamera view offset／film APIなどの互換作業は独立項目として維持する。

### データ境界

| 所有層 | 保持するもの | 巨大化への対応 |
| --- | --- | --- |
| Rust Document | 永続asset ID、配置、材質参照、編集差分、asset revision | 三角形ごとのObject化・全geometryのsnapshot複製を避ける |
| Asset store | version付きmanifest、粗いLOD、圧縮geometry page、空間階層、checksum | SSD／外部ストレージから必要範囲だけ読む。詳細metadata自体もページ化 |
| Rust worker | 読み込み・検証・展開・LOD生成job、要求優先度 | job数・bytes・同時実行数・CPU時間を有界化 |
| Residency manager | CPU／GPU page table、generation、pin／使用中情報、退避候補 | renderer／windowを跨ぐ重複保持を抑え、device／process単位の予算を管理 |
| wgpu Renderer | 常駐page、可視cluster、描画引数、粗い代替LOD | frameごとに処理するnode／cluster／bytesに上限を設ける |
| Tauri WebView | 操作・選択・進捗・budget状態・小さいエラーDTO | geometry本体・全Instance・フレーム画像の大量IPC転送を追加しない |

Universal-3Dとは固定形状bank・共有geometry・差分更新の責務を比較しながら進める。
参照した設計とThreeX固有の差異を[collaboration.md](collaboration.md)へ記録する。
シンボリックリンクへのimport／include／Cargo path／実行時依存は追加しない。

### G01：resource handle・asset ID・世代管理

🟢 [Done] G01-A：一時resource poolとcustom mesh接続。
🟢 [Done] G01-B：永続asset参照とDocument／worker／保存・結果受理制御面。
実geometry payloadの生成・読み込み・描画はG02へ進める。

- 永続asset IDと内容revision／hashをDocument側に持ち、GPU slot番号を保存形式へ埋め込まない。
- 🟢 [Done] 一時resource handleはpool epoch＋slot＋generationとし、解放・再利用後の古いhandleを拒否する。
- 🟢 [Done] slot上限とgeneration枯渇時の廃止、pool／Renderer再作成時のepoch分離。古いhandleが再び有効になる再利用を避ける。
- 🟢 [Done] pending予約の取消・遅延結果・二重publish拒否の共通pool。
- 🟢 [Done] workerのBegin／Finishにasset参照・Document guard・要求handleを接続し、削除・置換・Undo・再起動後の結果を拒否する。実loaderはG02。
- request再送の同一性とresource jobの同一性を分離し、既存の永続receipt／session終了保証を維持する。
- asset参照を永続化する際はcheckpoint／journalのversion移行・hash・旧形式読込を一緒に設計する。

完了条件：解放後のslot再利用、古いjob完了、device交換、複数window、Undo／Redo、
保存／再起動／request再送で誤参照・二重公開・リソース漏れが起きないCPUテスト。
性能比較は通常の1件差分更新・idle・gather cache再利用を対象とする。

### G02：粗いLOD・最小page形式・固定容量pool

🟢 [Done] G02-A：fixtureによる検証付きseek/read・固定pool・粗い表示計画・Renderer API。
🟢 [Done] G02-B：native worker／surfaceへの有界loader dispatch・完了配送・対象別redraw。
2 thread・8 lease、staging予約8MiB、古い結果拒否・終了drainの詳細：[native-asset-loader.md](native-asset-loader.md)。
⭕️ [Pending] 実GPU描画とreadback。実装・制限：[asset-pages.md](asset-pages.md)。

- 最初はテスト生成asset用の最小独自形式を使う。汎用importerや特定圧縮方式の完成を前提にしない。
- manifestにbounds、asset revision、page参照、root LOD、誤差指標、整合性情報を持たせる。
- offset／lengthは64bitで扱い、加算overflow・ファイル範囲外・異常な展開サイズを読み込み前に拒否する。
- 詳細pageには小さなtriangle clusterとlocal indexを格納する。pageサイズ・cluster数は計測して決め、wire定数として早期固定しない。
- 同じモデルの粗いroot LODを先に読み、詳細未到着・budget超過・読み込み失敗でも親LODで描画を継続する。
- 既存custom meshの同kind既定mesh fallbackは維持し、asset描画にはasset自身の粗いLODを使う別経路を用意する。
- rootもbudget対象に含める。rootだけで収まらない場合はbounds proxyと理由を返し、全assetのroot常駐を無条件に要求しない。
- CPU stagingとGPU page poolを固定容量で運用し、source geometry全体を一度にVecへ展開しない。
- メモリ上の小さなroot indexから開始し、詳細階層metadataもオンデマンドで読む。

完了条件：詳細pageなしで粗い表示計画を作れること、pageの追加・解放・再読込、容量0／上限ちょうど／超過、
破損manifest／欠落page／部分読み込み・展開失敗をCPU fixtureで検証する。
既存Box／Plane／Sphere・保存・差分転送を変更せずに共存させる。
実GPU表示は別のPending gateとし、CPUでのページ選択成功と混同しない。

### G03：階層LOD builder・空間階層・CPU可視選択

🟢 [Done] G03-A：G02のbounds／root・detailを使うCPU参照版のfrustum判定・LOD選択。instance単位のhysteresis、root／proxy fallback、疎な再判定とgather cache維持を実装（[詳細](cpu-lod.md)）。
🟢 [Done] G03-B1：asset内coverage階層・正確なcluster分割builder・instance空間階層と差分refit。現行root／detail Rendererへ接続（[仕様・検証](hierarchies.md)）。
🟢 [Done] G03-B2a：境界保持の親geometry簡略化と保守的error評価、node error envelopeとexisting pageへのroundtrip回帰（[仕様・検証](parent-simplification.md)）。
🟠 [Next] G03-B2b：多段page形式／loader／GPU接続と有界background builder／cache。G03-B全体は未完了。

- 🟢 [Done] G03-B1：元頂点を変更しないcluster分割と複数段coverage treeのCPU選択、容量不足／子欠落時の親維持。
- 🟢 [Done] G03-B2a：境界vertex／edgeを固定したendpoint縮約、元surface全体への対応に基づく誤差上限、各親を元triangleから直接生成。
- 🟠 [Next] G03-B2b：生成した多段pageの保存・配送へ接続する。
- 原形のbounding volumeと簡略化誤差を保持し、画面上の誤差を基準に親／子を選ぶ。
- 🟢 [Done] G03-A：既存のbounds／root・detailでCPU frustum判定・screen-space errorの選択を実装。hierarchy traversalはG03-Bで拡張する。
- ⭕️ [Pending] 実GPU画像のclip境界・LOD poppingとMacBook Air性能。Metal adapter NotFoundのためCPU検証と区別する。
- 🟢 [Done] 登録asset instanceの階層node単位の不可視除外とdirty leaf祖先のみのrefit。camera変更時のmetadata走査・draw plan再構築はO(instance数)で残る。
- 🟠 [Next] instance階層の訪問budget・再平衡と、asset内clusterの階層描画へ拡張する。
- 各枝は親または必要な子の集合で面を覆う。子が揃う前に親を消して穴を作らない。
- cluster境界の亀裂、異なるLOD間の継ぎ目、急な詳細度切替を検証し、境界保持とhysteresisを導入する。
- viewごとのnode訪問数・cluster数・triangle数・request数に上限を設け、打ち切り時は親へ戻す。
- 複数viewの要求を統合し、不可視view／休止windowによる不要な詳細pageの保持を抑える。

完了条件：同じcamera入力で決定的な選択、境界上のcamera、急移動、巨大bounds、異なるscale、
budget縮小でもcoverageを維持するCPUテスト。実画像の亀裂・popping確認はPendingで管理する。

### G04：非同期streaming・複数budget・退避

🔴 [Later] CPU参照選択に基づく要求を、有界workerへ接続する。

- page状態を未要求／待機／読込中／CPU常駐／GPU常駐／退避待ち／失敗などで明示し、重複要求をまとめる。
- root／画面上の誤差改善が大きいpageを優先し、camera移動予測による先読みは余剰budget内に限定する。
- 読み込み中bytes、展開済みCPU bytes、GPU geometry bytes、metadata bytes、texture bytesを別々に制限する。
- CPU／GPUが共有物理メモリを消費することを考慮し、合計budgetと重複copy・一時展開の上限も設ける。
- geometry poolはbuffer内のsuballocationを基本とし、alignment、断片化、再利用、世代付きpage tableを管理する。
- 使用中pageとcoverageに必要な親をpinし、GPU submission完了前に領域を上書きしない。
- 退避済みだが実行中GPUから参照されるbytesを別計上し、単なる論理dropで空き容量を過大評価しない。
- LRUだけでなく、最近の可視性・再読込コスト・LODのcoverageを使って退避候補を選ぶ。
- 遅いSSD・外部ドライブ切断・容量不足・破損pageには有界retryと親LOD fallbackを使う。描画threadでI/O完了を待たない。
- rendererごとのbudgetに加え、複数window／deviceの合計上限と共有asset cacheを設計する。

完了条件：読み込み／展開jobのfault injection、途中cancel、asset交換、window終了、device lossで漏れ・誤公開なし。
容量を超える要求を繰り返しても常駐量とqueueが上限内で安定し、thrashingの回数を測定できること。

### G05：GPU culling・indirect draw・occlusion

🔴 [Later] G03のCPU参照版とG04のresidencyを維持してGPUへ移す。

- computeでInstance／clusterのfrustum cullingとLOD選択を行い、可視リスト・描画引数をGPU内で生成する。
- storage binding／dispatch上限に合わせて分割し、固定容量queue・可視リストのoverflow時は粗いLODへ退く。
- capabilityを確認してindirect drawを使い、非対応時はCPU選択＋通常drawへfallbackする。
- readbackを毎frameの必須経路にせず、統計・streaming要求の読出しも遅延可能な有界bufferで行う。
- 次段階でHi-Z occlusionを追加する。camera急移動や遮蔽物変更時は保守的に扱い、誤った不可視判定を避ける。
- 既存の差分Instance bank／gatherを流用できる経路は維持し、GPU専用最適化のために全Instance再送へ戻さない。

完了条件：CPU参照版との可視判定比較、上限超過・非対応limits・device lossのfallbackテスト。
⭕️ [Pending] 実GPUのreadback一致、indirect／Hi-Z描画の正しさ、GPU時間。
10万cube比較では全描画／CPU culling／GPU culling／LOD＋streamingの差を、同じ入力で測る。
反復cubeの結果だけで固有geometry数千億ポリゴンへの対応を主張しない。

### G06：圧縮・共有・品質の自動制御

🔴 [Later] 基本streamingの正しさを維持した追加最適化。

- 同じassetはgeometryを共有し、Instanceは配置・必要な状態だけ保持する。materialも参照共有する。
- 頂点の局所量子化、local index、mesh圧縮、texture mip streamingを段階導入する。
- 圧縮率だけでなく展開時間・一時メモリ・誤差・random access効率を比較して方式を選ぶ。
- 計測frame時間に応じてscreen-space error・描画解像度・先読み量・frame当たりupload量を調整する。
- 品質変更には上下限とhysteresisを設け、負荷変動でLOD／解像度が往復し続けないようにする。
- 操作中は応答性、停止後は詳細化を優先し、idle時は既存on-demand schedulerで描画を停止する。
- 電池動作／低電力設定／長時間負荷で使う品質profileを用意し、機種名だけで固定品質を決めない。
- Metal固有のheap等はwgpu経路の計測後に検討し、必須機能にせずportableなbuffer poolを維持する。

完了条件：圧縮前後の誤差・容量・展開時間比較、品質制御の有界性、idle停止と差分更新の回帰テスト。
⭕️ [Pending] MacBook Air上の画質・電力・発熱・長時間のframe安定性。

### G07：選択・編集・解析・保存のout-of-core化

🔴 [Later] 描画以外の処理が全geometry展開を要求しないようにする。

- pickingは空間階層で候補を絞り、正確な判定が必要な候補pageだけ読む。待機中は近似結果と処理状態を区別する。
- 編集は変更pageと影響する祖先LODだけを再生成し、未編集pageを共有する。
- asset内容はimmutable revisionとして公開し、編集中の未完成pageを描画側へ公開しない。
- Document transactionは新asset revision参照と編集結果を原子的に公開し、request再送で再適用しない。
- page書込→検証→manifest公開→Document参照の順序と復旧を設計し、途中終了で参照不能な完成状態を作らない。
- Undo／Redo・checkpoint・未完了job・実行中renderから参照されるasset revisionをGC対象から除外する。
- 全体解析・exportはpage単位のbatch処理とし、進捗・中断・再開・メモリ上限を持たせる。
- 重い前処理を別マシンへ移す場合もasset形式とjob APIで分離し、MacBook Air単体での閲覧を維持する。

完了条件：変更領域の限定、Undo／Redo、再送、保存中断・末尾破損・process exit復旧、
GCと履歴参照・読み込みjobが競合しても必要assetを失わないテスト。

### G08：規模を分けた評価と、実機の合格基準

🔴 [Later] 性能目標は以下の測定手順で段階的に判定する。数値は実測前の検証目標。

| 段階 | データ | 確認すること |
| --- | --- | --- |
| A | 小規模・全件展開できるfixture | 参照画像／CPU結果、LOD coverage、page lifecycle、破損時の復旧 |
| B | 100／1k／10k／100k Instanceと共有asset | 既存差分更新・gather cache・draw数・CPU時間の回帰 |
| C | 設定した常駐budgetを超える固有geometry | 全件読込なし、常駐量・queue・upload量の上限、親LOD fallback |
| D | SSD容量に収まる複数規模の固有dataset | 初回表示・camera経路・cache hit／miss・seek・thrashingの比較 |
| E | 数千億三角形相当の論理scene | Instance反復と固有assetを分け、階層metadataが全常駐しないことを確認 |
| F | 実際の数千億三角形の固有dataset | 保存bytes・圧縮率・読込bytes・参照画像を伴う実証。論理scene試験で代替しない |

計測ではMacBook Airのchip／GPU、統合メモリ容量、macOS、電源・低電力設定、解像度、
SSD／外部ストレージ、データhash、build設定、budget、cacheのcold／warmを記録する。
定義済みcamera経路を同条件で複数回走らせ、中央値だけでなくp95／p99・最大値も保存する。

- CPU：frame準備、階層node訪問数、LOD選択、展開時間、変更slot数、copy／gather命令数。
- GPU：frame時間、実描画triangle／cluster／draw数、可視リストoverflow、upload bytes。
- メモリ：CPU staging、metadata、geometry／texture pool、pin中／退避待ちbytes、process footprint、memory pressure／swap。
- I/O：要求数、読込bytes、latency、取消・再要求、cache hit率、圧縮／展開bytes。
- 操作性：初回の粗い表示までの時間、選択応答、camera移動時の停止、LOD穴・popping、idle復帰。

⭕️ [Pending] 実機で初期目標30fps（約33.3ms/frame）を評価する。達成を保証せず、
解像度・dataset・品質profileを固定して結果を判定する。60fpsはその後の追加目標。
⭕️ [Pending] 少なくとも20分の連続camera移動と停止を含む試験で、熱による速度変化・memory pressure・
品質の過剰な往復・無制限queue増加を確認する。空きメモリに応じたbudgetの具体値はこの測定から決める。
⭕️ [Pending] Metal adapterが利用できる環境でGPU readback／画像比較を行い、WASM／browserは別経路として検証する。
CPUでのbytes照合・合成datasetの成功だけでは、MacBook Airの実描画や数千億固有ポリゴン対応をDoneにしない。

### この計画の参考と3言語要約

階層LOD・細粒度streamingの参考：[Epic Nanite公式資料](https://dev.epicgames.com/documentation/en-us/unreal-engine/nanite-virtualized-geometry-in-unreal-engine)。
Metal固有のメモリ再利用の参考：[Apple Resource Heaps](https://developer.apple.com/library/archive/documentation/Miscellaneous/Conceptual/MetalProgrammingGuide/ResourceHeaps/ResourceHeaps.html)。
これらの製品・APIを直接採用済みという意味ではなく、ThreeXのRust／wgpu経路へ段階的に設計を適用する。

English: The goal is bounded, interactive access to datasets containing hundreds of billions of triangles,
not simultaneous rendering of all triangles. Generational mesh handles and persistent asset references are implemented. The minimal coarse LOD/page-pool/Renderer API is implemented. Bounded native loader dispatch and target-specific redraw are implemented. CPU bounds/frustum and screen-space LOD selection with hysteresis are implemented. Next: hierarchical cluster building and selection. Later stages add hierarchical selection, asynchronous streaming,
GPU culling/indirect draws, compression, adaptive quality and page-local editing/recovery. Report unique
geometry separately from instanced logical counts. MacBook Air frame times, memory pressure, sustained
performance and actual GPU correctness remain pending until measured on a specified device.

简体中文：目标是在固定内存和每帧工作量预算内交互访问数千亿三角形数据集，而不是同时绘制全部三角形。
已实现generation资源句柄和持久asset引用。已实现最小粗LOD和page pool的Renderer API；已实现native有界loader配送和redraw连接；已实现CPU可见性与屏幕误差LOD选择和迟滞；下一步是层级cluster构建与选择；后续依次加入层级选择、异步流式读取、
GPU剔除／间接绘制、压缩、自适应质量、局部编辑和故障恢复。实例重复的逻辑数量与独立geometry数据量分别统计。
MacBook Air实机帧时间、内存压力、持续负载和GPU正确性在完成指定设备测试前保持Pending。

## Verification results

- Native workspace compilation, formatting and Clippy: passed.
- Rust core: 72 passed; storage: 71 passed; renderer: 43 passed; Tauri host: 18 passed in debug and release. Tauri IPC / native scheduler: 18 passed. Renderer lifecycle: 5 passed; WGSL validation: 2 passed.
- Current native suite: 204 passed in debug and release; JS: 13 passed.
- JS compatibility tests: 13 passed.
- Native GPU pipeline / gather readback tests: attempted; Metal adapter NotFound. Explicitly ignored in normal tests.
- WASM: target install failed; wasm-pack not installed. Browser path remains unverified.

## 関連プロジェクトとの協調

- 🟢 [Done] Universal-3Dとの役割・データ境界・互換性の差異をcollaboration.mdに記録。
- 🟢 [Done] Surface Lost/Outdatedへの復旧処理をWASM Runtimeに追加。
- ⭕️ [Pending] Surface復旧経路のWASMビルドと実機確認。

- 🟢 [Done] Universal-3D conversion and world mirror verified through the new incremental SoA path.

Current upload-preparation comparisons and limits: upload-performance.md; raw data: benchmarks/upload-\*.csv.

Current membership packing measurements and limitations: packing-performance.md; raw data: benchmarks/packing-\*.csv.

## Four設計の統合

- 🟢 [Done] ThreeX互換APIを維持してFourのRust所有・取引編集・有界履歴を採用。
- 🟢 [Done] Documentサービス：revision競合検出、Undo/Redo、安定ID復元、明示読取snapshot。
- 🟢 [Done] FX-01〜06の日本語・英語・简体中文仕様と実装対応表（four/README.md）。
- 🟢 [Done] version付きDocument checkpoint、Undo/Redo永続化、世代復旧、障害・プロセス終了テスト（storage 30件）。
- 🟢 [Done] 意味内容hash v1、snapshot／保存receipt／worker明示query、正規化と保存・復旧検証（four/content-hash.md）。
- 🟢 [Done] Tauri asyncホスト接続・window/origin/session認可・3言語エラーDTO（four/tauri-ipc.md）。
- 🔴 [Later] WorkspaceStateと評価World分離、取引的surface再接続。
- ⭕️ [Pending] OS別native surface・ドッキング・GPU喪失の実機検証。

- ⭕️ [Pending] 保存の実電源断・容量不足・Windows／Linux durability検証。

- 🟢 [Done] AutosaveDocument：commit／Undo／Redoの自動journal、末尾の最後の正常transaction復旧。
- 🟢 [Done] journal append／圧縮の中断、全切断位置・破損、process exit・追記再開のテスト。
- 🟢 [Done] journal差分transaction prepare/apply、全Document複製廃止、CPU／fsync込み前後比較。
- 🟢 [Done] bounded group commit、1syncで複数revisionを公開、正常prefix復旧・前後性能比較。
- 🟢 [Done] Tauri async接続、有界応答待機・受理後の中断／終了drain。
  最新仕様と制約：four/autosave-journal.md。

最新の差分journal性能と制約：four/journal-staging-performance.md。

Group commit仕様・計測・制約：four/group-commit-performance.md。

- 🟢 [Done] group累積prefixの再検証・再適用を変更ID overlayへ差分化、障害復旧維持、前後性能比較。
- 🟢 [Done] 構造編集の階層検証／隣接表を変更経路・辺の差分へ置換、復旧と性能比較。
  最新のgroup準備仕様・計測：four/group-prefix-performance.md。

最新仕様・性能・残るコスト：four/structure-performance.md。

- 🟢 [Done] Rust Document worker、有界queue、時間／件数batching、session内request結果cache・再送拒否。
- 🟢 [Done] worker7テスト、3言語error、headless実行例。
- 🟢 [Done] Tauri async接続、Rust worker単一所有とrestart再送検証。
- 🟢 [Done] Rust Renderer接続とnative surface lifecycle：worker Scene借用・世代付きtarget・resize／休止／再接続／破棄。
仕様と保証範囲：four/worker.md。

- 🟢 [Done] 永続Document ID、checkpoint／journal v2とv1移行、request/session protocol v1。
- 🟢 [Done] 編集＋receiptの原子的記録、restart／Undo／圧縮後の再送拒否、watermarkと期限切れ。
- 🟢 [Done] Tauri IPC接続、永続window session割当と再送重複排除。
仕様・保証範囲：four/request-protocol.md。

- 🟢 [Done] ProtocolRequest Commitのgroup commit、worker自動併合、1syncで編集＋receiptを公開。
- 🟢 [Done] restart／正常prefix復旧・再送抑止、no-op圧縮中断、fsync回数と前後2組の性能比較。
仕様・実測・制約：[four/protocol-group-performance.md](four/protocol-group-performance.md)。

- 🟢 [Done] worker ProtocolRequest候補prefixの差分検証、受理済み準備結果の直接journal送信。
- 🟢 [Done] 不正候補後のprefix維持・restart重複排除・障害復旧、候補CPU／実workerの前後比較。
最新の仕様・性能：[four/worker-prefix-performance.md](four/worker-prefix-performance.md)。

- 🟢 [Done] 裸Commit worker候補prefixの差分検証、prepared group直接journal送信。
- 🟢 [Done] no-op／transport再送cache／障害復旧維持、候補CPU・実worker前後比較。
最新仕様・実測：[four/bare-prefix-performance.md](four/bare-prefix-performance.md)。

- 🟢 [Done] session lifecycle：明示終了・Active枠再利用・永続終了記録、checkpoint／journal v3とv1／v2移行。
- 🟢 [Done] 終了後／restart／receipt淘汰後の再送抑止、書込／圧縮中断・process exit復旧。
仕様・上限・検証：[four/session-lifecycle.md](four/session-lifecycle.md)。

- 🟢 [Done] 受理済み編集エラー結果の永続化・当時の結果／revisionの再送、失敗時の連番消費。
- 🟢 [Done] checkpoint／journal v4・旧v1〜3移行、restart後の同一結果・重複実行防止・障害復旧。
仕様と受理境界：[four/durable-errors.md](four/durable-errors.md)。

- 🟢 [Done] Tauri MockRuntimeのIPC／native接続17テスト：認可・DTO・共有所有・永続失敗・restart再送・session終了。
- ⭕️ [Pending] 実Wry WebView／OS別IPC設定とGUIの検証。
仕様・接続例：[four/tauri-ipc.md](four/tauri-ipc.md)。

- 🟢 [Done] native wgpu Surface backendとTauri Window接続、Lost／Outdatedの有界再試行、Device Lost／OOMの描画側隔離。
- 🟢 [Done] native lifecycle 5テスト＋実invoke追加2テスト、Scene単一所有・GPU障害後の編集保存・restart再送重複排除。
- ⭕️ [Pending] 実OS surface／Wry表示とDPI・GPU喪失、製品UI・ドッキング。
仕様・API・検証境界：[four/native-renderer.md](four/native-renderer.md)。

- 🟢 [Done] native Window callback自動配線、worker公開後のScene変更通知、on-demand redraw scheduler。
- 🟢 [Done] 最新サイズ／camera集約、backpressure時の閉鎖保持、休止・再開、再試行上限・公平な複数target処理。
- 🟢 [Done] 追加6テスト：idle停止・描画中変更・応答破棄・GPU失敗・restart成功／エラー再送と非重複描画。
- ⭕️ [Pending] 実Wry／OSのイベント配送・実GPU表示とexpose／hide-show・DPI検証。
仕様・接続・検証境界：[four/native-redraw.md](four/native-redraw.md)。

- 🟢 [Done] camera CPU計算・互換テスト追加（Rust 3／JS 5）、既存Document／native scheduler回帰を検証。
- ⭕️ [Pending] 新WASM camera bindingのtargetビルド・browser／GPU表示検証。
仕様と制約：[camera.md](camera.md)。

- 🟢 [Done] geometry・分割flags、checkpoint／journal v6とv1–5移行、content hash v2。
- 🟢 [Done] Box・混在形状の差分描画計画、形状のみの更新・history・末尾復旧・再送、writer unlockの回帰検証。
- ⭕️ [Pending] 3形状の実GPU／WASM表示。Metal adapter取得不可。
仕様・制約：[geometry.md](geometry.md)。

## MuTracerの設計参照

- 🟢 [Done] 一時配置されたMuTracerの設計資料を確認し、[設計参考方針](mutracer-reference.md)を記録。コード・shader・テスト・生成物・assetは流用せず、配置ディレクトリへの依存も追加しない。
- 🟠 [Next] G03-Bでasset内LOD階層とinstance空間階層、boundsの差分更新、有界background builderを独立設計する。レイ探索用BVHをLOD階層としてそのまま扱わない。
- 🔴 [Later] G04／G06／G07で構築cache・品質制御・局所編集の設計参考にする。既存の差分更新・gather cache・root／proxy fallbackは維持する。
- ⭕️ [Pending] 同条件の実GPU描画・性能比較。今回MuTracerのビルド・実行・性能測定は行っていない。
