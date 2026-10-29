# G03-A: CPU bounds and screen-space LOD

現在は[G03-B1の独立した階層](hierarchies.md)を接続し、instance空間階層で候補を絞ってから以下のlocal bounds／SSE判定を行う。不可視なdirty instanceはbounds_testsが0になる場合がある。

🟢 [Done] Registered asset instances use CPU frustum rejection and screen-space error (SSE) to choose G02 root/detail pages. `Renderer::set_cpu_lod_policy` controls the pixel threshold (default 2), hysteresis ratio (default 0.2), and frustum rejection. `cpu_lod_status` reports candidates, visible/culled instances, detail demand, bounds tests and draw ranges. These are runtime view settings; Document, checkpoint v7, journal v7 and protocol v1 are unchanged.

## 判定と描画

描画と同じcolumn-major view-projection／world model行列を使う。WebGPUのclip空間はXYが−w〜w、Zが0〜w。asset manifestのlocal AABBに6つのclip平面を適用し、AABB全体が平面外にあると証明できたinstanceだけを除外する。計算はf64、境界にはf32丸めの余裕を入れる。負scale、非一様scale、回転、親由来のshearも合成行列で扱う。

root pageのlocal-space errorを、透視除算の分母の下限と行列の線形成分のnormからpixel変位の保守的上限へ変換する。viewportは物理pixel単位で、zoom／aspectは描画用VP行列に含まれる。orthographicにも同じ式を使う。宣言されたerrorの正しさ自体を検証するbuilderはG03-Bの範囲。

誤差がthresholdを超えるとdetail、threshold × (1 − hysteresis)以下になるとroot、その間は前回の選択を維持する。cameraがnear／far境界を横切る場合やwの下限が安全でない場合は誤差を無限大としてdetailを要求する。無効なcamera・bounds・modelも可視性不明として保持する。これによって不確かな判定でgeometryを消さない。無効なpolicyはsetterで拒否する。

選択はinstanceごとに行い、同じassetでも近景detailと遠景rootを混在できる。detail未到着・読み込み失敗・容量超過時はroot、root未到着時はbounds proxy。ページ到着や失敗によってDocumentやInstanceを編集しない。frustum外のassetはprimitive代替描画からも除外し、元のBox等が漏れて表示されることを防ぐ。

## 差分更新とコスト

camera／viewport／policy変更では登録asset候補を走査する。同じviewではInstanceのdirty rangeに含まれる登録候補だけを再判定し、変更のないframeやpage到着だけのframeはbounds判定ゼロ。asset membership／登録manifest変更時は候補metadataを作り直す。可視性・LOD・membershipが変わった場合のみsource順のdraw rangeを再構築する。membership再構築時にはhysteresis履歴も初期化する。

CPU計画はsource Instanceを複製せず、可視性変更によるGPU Instance再パッキングも行わない。既存のGeometryBatches、差分転送、分割gatherとresident索引cacheをそのまま利用する。camera変更はそれらのrevisionや転送rangeを変更しない。CPU側には候補ごとのmetadataを保持する。

現在の対象はboundsが登録されたassetであり、通常のBox／Plane／Sphereや未登録assetは既存の描画経路を通る。camera変更はO(登録asset instance数)、選択変更時のdraw計画再構築もO(候補数)。空間階層での枝刈り・訪問budget・多段LODはG03-B。viewに応じたpage読み込み優先度・自動退避はG04であり、現在のloaderへの明示的なroot/detail要求は維持する。

## Verification

CPUテストは6平面、clip境界、不正値、透視投影の距離、viewport、反転scale／shear、独立に投影した1000サンプルに対する誤差上限を検証する。Rendererテストは同一assetのroot/detail混在、hysteresis、無効camera、culling無効化、局所dirty、membership swap、proxy/root/detail fallbackと破損detailを検証する。1200 instanceの交互bank配置でcamera変更後もInstance転送ゼロとgather索引の再送なしを検証する。既存のpage pool容量・解放・stale結果拒否・保存・再送重複排除テストも維持する。

Rust workspaceはdebug／releaseとも191 passed（core 63、renderer 39、storage 71、Tauri host 18）、JS 13 passed。Clippy all-targets／all-features、format、diff checkも通過。release初回は既存のresize集約テストが途中サイズを観測して失敗したが、単独再実行とworkspace全体の再実行は通過した。時間依存の揺らぎとして記録し、G03-Aの変更に伴うテスト失敗と混同しない。

⭕️ [Pending] Native Metal adapterを利用できない開発環境では、実GPU描画・画像の境界／popping・MacBook Airの性能は未検証。GPUテストにはcamera cullingと静止viewのbounds判定ゼロの確認を追加した。CPUテストやWGSL検証を実画像の検証とは扱わない。

Universal-3Dの`object_bounds`／`frame_selection`とshearを含む保守的bounds処理を読み取り参照した。ThreeXでは独立したclip-space AABBと投影誤差上限を実装し、symlinkへの依存・import・ビルド参照を追加しない。

English: CPU selection uses the rendering matrices, conservative AABB rejection and projected error bounds. Per-instance hysteresis selects root/detail while the existing page pool supplies root/proxy fallback. Sparse updates and resident gather indices remain unchanged. Hierarchical traversal and view-driven streaming follow in G03-B/G04; actual GPU validation remains Pending.

简体中文：已实现基于绘制矩阵的CPU包围盒／视锥判定和屏幕误差LOD选择。每个实例独立选择root或detail，迟滞减少频繁切换，未加载或失败时保留root／proxy。局部更新和gather索引缓存保持不变；空间层级和视图驱动流式加载后续实现，实机GPU验证保持Pending。
