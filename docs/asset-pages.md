# G02-A：最小asset page・粗いLOD・固定容量pool

🟢 [Done] 検証付きfile reader、root／detailの2段階LOD、固定容量PagePool、RendererのRust APIと描画経路。
🟢 [Done] [G02-B](native-asset-loader.md)：native Document worker／surfaceへの有界loader dispatch・完了配送・対象別redraw。
⭕️ [Pending] 実GPUでのproxy／root／detail表示、メモリ解放と性能、WASM／browser。

現段階は小さなfixtureをend-to-endで扱う最小構成。汎用importer・自動LOD生成・階層traversal・
自動disk streaming schedulerは未実装。Sceneにasset参照を付けるだけで自動ロードは始まらない。

## Container v1

`core::asset_pages::AssetManifest`はroot 1ページと任意のdetail 1ページを持つ。
各ページはasset全体を覆うmeshで、detailはrootを丸ごと置き換える。
部分的に到着した子clusterを混在させる形式ではない。細かいcluster階層はG03で導入する。

| manifest領域 | bytes | 内容 |
| --- | ---: | --- |
| magic | 8 | `THREEXA1`。別versionは明示拒否 |
| AssetId | 16 | 非zero、Document参照と一致 |
| asset revision | 8 | u64 LE、Document参照と一致 |
| local bounds | 24 | min XYZ、max XYZ、6個のf32 LE |
| file length | 8 | u64 LE、実fileサイズと一致 |
| page count | 4 | u32 LE、1または2 |
| reserved | 4 | 全zero |
| page descriptor | 60 × count | 以下の表。root→detail順 |

| descriptor領域 | bytes | 内容 |
| --- | ---: | --- |
| offset／length | 各8 | u64 LE。checked add・範囲・順序・非重複を検証 |
| vertex／index数 | 各4 | u32 LE、各3以上、index数は3の倍数 |
| error | 4 | finite・非負f32。detailの値はroot以下 |
| payload hash | 32 | raw page bytesのSHA-256 |

manifestは132または192 bytes。先に固定header 72 bytesだけ読み、count検証後に残りdescriptorを読む。
`AssetRef.hash`はmanifest bytesのSHA-256とし、各descriptorのpage hashを介してpayloadも結び付ける。
Document ID／revision／期待hashの形式はG01-Bのまま。checkpoint／journal v7・内容hash v3を変更しない。

raw pageはpositionのf32×3配列、その後にlocal u32 index配列を持つ。
lengthは`12 * vertices + 4 * indices`と一致させ、1ページ上限は1MiB。
allocation前にサイズを検証し、入力hash・有限座標・bounds内・index範囲を検証する。
検証後は`ValidatedPage`が単一bufferを所有する。余分なVec容量は保持せず、全asset複製や展開を行わない。
圧縮・UV／normal／material page・skin／animationはこのv1の対象外。
errorはbuilderの申告metadataであり、幾何学的な近似誤差の算出／保証はG03のbuilderで扱う。
manifest／pageの将来拡張ではversionを更新する。

## 読み込みと結果受理

`storage::asset_pages::AssetReader`はnativeのseek/read helper。
`open(path, reference)`でmanifestと実fileサイズを検証し、`read_request(request)`で指定ページだけ読む。
requestのasset参照がreaderと異なればread前に拒否する。
1回のpayload allocationは検証済みの1MiB以下で、file全体をread_to_endしない。
missing file・途中切断・短いread・hash／geometry不正はtyped errorで返す。

callerはRust側で次の順に実行する。これはAPI接続例であり、現在のworkerが自動起動する処理ではない。

1. DocumentのAssetRefに対応するAssetReaderを開く。
2. Rendererへmanifestを渡し、`request_asset_page(&document, entity, manifest, level)`で容量を予約する。
3. `PageAdmission::Load(request)`なら、描画処理の外で`reader.read_request(request)`を行う。
4. Rust側へ戻し、`complete_asset_page(&document, request, result)`で検証・GPU uploadする。
5. read errorは`PageFailure::ReadFailed`へ変換して完了させ、予約を解放する。失敗後の再試行は新しいrequestで行う。
6. `Resident(handle)`なら再read／再uploadせず既存resourceを使う。

poolは1つの永続Document IDに属し、別Documentはclear前に受け付けない。
Document identity／revision、entity参照、AssetRef、pool epoch／slot generation、page level／lengthを検証する。
取消・置換・Undo／RedoのABA・再起動・pool再作成・重複完了後の結果はfactory／GPU allocation前に拒否する。
異なるDocumentへ誤配送された完了は、元の予約を消費しない。
既に検証済みのresidentはimmutableなAssetRefで識別し、編集後に同じ参照が必要なら再利用できる。

readerは同期helperであり、Renderer::renderからI/Oを呼ばない。
現在は呼出側がloader jobの同時実行・結果回収・redrawを制御する。
テストでは別Rust threadで読み、Document編集後に完了を配送して古い結果拒否を確認する。
G02-Bでnative hostの有界dispatch・取消入力の回収までのlease追跡を接続済み。G04で優先順位・先読み・合計budgetへ広げる。

## 容量・解放・fallback

`renderer::asset_pages::PagePool<T>`はconstructorのbyte数／page数を上限とし、readyとpendingの両方を計上する。
rootもbudget対象。detailを要求する前にrootのresidentが必要。
rootはdetailより優先して保持し、detailの失敗や容量超過でrootを退避しない。

| 状況 | 選択／動作 |
| --- | --- |
| root未読込・rootが入らない | manifest boundsのproxy |
| root resident、detail未到着／失敗／上限超過 | root mesh |
| detail検証・upload完了 | detail meshへ置換 |
| detailの解放 | rootへ戻す |
| poolのbudget変更／clear | page handle失効。登録済みmanifestが残る場合はproxy |
| 初回manifestすら読めない／Renderer manifest上限超過 | typed error。未登録assetは既存primitive表示を維持 |

`cancel`はpending予約を解放、`evict_detail`はrootを維持してdetailだけ解放、
`release_asset`はpool内のroot／detailを解放、`clear`は全handleを失効させる。
解放容量とslotは新しいgenerationで再利用する。pending予約だけで容量を占有したままにならないようにする。
未使用assetの自動LRUや参照GCはまだ行わず、呼出側が明示解放する。

Rendererのpage budgetは初期64MiB／128ページ。`set_asset_page_budget(bytes, pages)`で変更できる。
同じ設定はno-op、変更時は固定poolを作り直してpending／residentを失効させる。
custom primitive meshのbudgetとは別であり、両者の合計をprocess全体の上限とは扱わない。
GPU pageはraw vertex／u32 index bufferとしてuploadし、各bufferのdevice上限も確認する。

manifest／proxyは最大128 asset。proxyは既定Boxの8頂点をboundsへ配置し、各168 bytesのGPU payloadを使う。
この最大約21KiBのproxy payloadと小さいmetadataはpage byte budgetの外側にある固定枠。
`Renderer::release_asset`はmanifest／proxyも登録解除し、そのassetはprimitive placeholderへ戻る。
`Renderer::clear_assets`は全登録とpageを解放する。

pool byte数は論理的なpayload上限であり、allocator／driver overhead・in-flight submissionの遅延解放は含めない。
GPU upload中はCPU入力とGPU出力が一時的に重なる。
取消後も呼出側のI/O jobが保持する入力bufferは、poolとは別にjob ownerが回収する必要がある。
全staging／退避待ちbytesの合計管理・物理buffer suballocationはG04で実装する。

## 描画・差分更新・gather

Rendererは登録済みassetのInstanceをprimitive drawから除外して、同じ既存Instance bufferのmodel／RGBでpageを描く。
proxy／root／detailは同じlocal座標系を使い、primitiveとの二重描画をしない。
同じassetで連続するsource Instanceはrangeをまとめる。交互asset配置の最適なasset別gatherは今後の改善対象。

Sceneは疎なasset参照のiteratorとasset layout stampを持つ。
asset参照／可視mesh membership／packed移動、bank revision、direct/banked遷移、manifest登録変更時だけdraw範囲を再計画する。
通常のRGB／Transform変更ではasset layoutを全走査せず、従来のdirty slotだけを送る。
pageの到着・失敗・退避はSceneのInstanceを再packingせず、既存gather計画／GPU索引も無効化しない。
登録していないassetと既存Box／Plane／Sphereは従来の描画経路のまま。

## 検証と残作業

- 🟢 [Done] workspace debug／release：179件（core 59、renderer 36、storage 67、Tauri host 17）。
- 🟢 [Done] JS 13件、全target／feature Clippy、fmt、diff whitespace検査。
- 🟢 [Done] manifest／page全切断位置、unknown version、count過大、offset overflow、異常length・counts・bounds・index。
- 🟢 [Done] manifest／payload hash、checksumが正しくても不正なgeometryの拒否。
- 🟢 [Done] byte／page上限0・上限ちょうど・超過、pending重複予約の集約、read失敗後の予約回収。
- 🟢 [Done] root→detail→root→proxy、解放・再読込、古いhandle拒否、別Documentの誤配送保護。
- 🟢 [Done] 別threadでの読込後のUndo／Redo ABA・再起動・cancel／clear・二重完了でfactoryを呼ばない。
- 🟢 [Done] 実file seek/read、欠落・途中切断・破損detailでもresident rootを維持する。
- 🟢 [Done] primitive除外rangeの境界、assetのkind変更・hide/show・packed index移動、RGB変更時の計画再構築なし。
- 🟢 [Done] 1 slot差分・idle 0・gather索引再送なし、既存保存／再起動／receipt・障害復旧の回帰。
- ⭕️ [Pending] native GPU testをproxy／root／detail・budget再作成・退避へ拡張したがMetal adapter NotFound。実GPU表示・readback・性能、WASM／browserは未検証。
- 🟢 [Done] G02-B：native worker／surfaceの有界loader dispatchとredraw。trusted Rustの明示LoadAsset要求から自動配送。
- 🟠 [Next] G03-AのCPU可視判定・LOD選択。
- 🔴 [Later] G03の多階層cluster／CPU可視判定、asset交互配置のbatching、G04の優先streaming・合計budget。

Universal-3Dのmesh入力検証・共有Batchを参照し、ThreeXは全triangle保持ではなくページ単位の検証／常駐へ分けた。
リンク先の型・保存形式・build／実行時依存は追加していない。

English: G02-A implements a bounded two-level asset container, SHA-256-verified seek/read, fixed-capacity page
residency and a Rust Renderer path selecting detail, root or bounds proxy. Pending bytes count toward capacity;
stale completions cannot allocate resources. Primitive exclusion and cached layout stamps preserve sparse uploads
and gather-index reuse. Native host loader dispatch is implemented in G02-B; actual GPU display/readback is Pending.

简体中文：G02-A实现两级asset格式、SHA-256校验的按页读取、有界page pool及Rust Renderer绘制路径，按详细／root／bounds proxy回退。
pending也计入容量，过期结果在资源生成前被拒绝。保留差分上传及gather索引复用，普通RGB变化不重建asset布局。
G02-B已实现native host的有界loader配送；实际GPU显示与回读保持Pending。未增加符号链接依赖。
