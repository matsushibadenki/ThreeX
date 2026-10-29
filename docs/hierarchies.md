# G03-B：asset階層とinstance空間階層

🟢 [Done] G03-B1：独立したasset内coverage階層、元geometryを保持するcluster分割builder、world-space instance空間階層と差分refit、現行root／detail描画への接続。

🟢 [Done] G03-B2a：[境界保持の親geometry簡略化と保守的error評価](parent-simplification.md)。

🟠 [Next] G03-B2b：多段page形式・loader・GPU描画への接続、有界background builder／cache。G03-B全体の完了ではない。

## Asset内の階層

`threex_core::lod_hierarchy::LodHierarchy`はasset座標系のbounds、誤差、page ID、子nodeを保持する。instanceの行列・可視性・GPU所有権を持たない。親を先に置く単一rootのtreeを検証し、循環・孤立・二重所有・親bounds外の子・親より大きな子誤差・不正値・node上限超過を拒否する。

CPU選択は親を、直接の子が**すべて常駐**している場合にのみ置き換える。node訪問数とdraw数の上限に収まらない置換も行わない。選択結果は各枝の親または子の集合となり、途中の置換失敗で穴を作らない。rootがない場合、または訪問／draw予算が0の場合は空のcutを返す。呼び出し元はbounds proxy等の代替を扱う必要がある。CPU APIのcut予算はその代替描画の数を含まない。

構造検証だけでは、子の三角形が実際に親と同じ面を覆うことや、宣言errorの正しさは証明できない。外部metadataの作成者はその契約を満たす必要がある。現在のTHREEXA1 adapterは同じboundsのroot／detailを階層化する。既存のhysteresisによるdetail要求を階層選択へ渡し、PagePoolの常駐状態から実際のroot／detail／proxyを選ぶ。metadataはAssetRefごとにcacheし、camera変更やpage到着だけでは作り直さない。

## 正確なcluster分割

`ClusterHierarchy::build`は借用した頂点・三角形から、長いbounds軸のcentroid中央値を使って決定的に分割する。同値の場合は元triangle IDで順序を決める。leaf内triangle数、入力triangle数、node数に上限がある。参照index・有限値を確認し、上限超過をエラーにする。生成途中に失敗したtreeを公開しない。

全nodeは単一のtriangle順序配列のrangeを参照する。子rangeは親rangeを重複なく分割し、各元triangle IDは配列に一度だけ格納する。頂点の移動・溶接・間引きは行わず、境界を含む元geometryを保持する。親は同じ正確なtriangle集合を表すためerrorは0。このbuilder単体で描画triangle数は削減されない。汎用LOD treeは複数段の誤差付きnodeを扱えるが、このexact builderから親geometryを生成するAPIは[別途実装した](parent-simplification.md)。多段page配送と自動background生成はG03-B2bに残す。

builderは明示呼び出しのCPU APIで、Document workerやrender内から自動実行しない。node metadataはO(node数)、triangle順序配列はO(triangle数)。中央値の再sortを用いる現実装の構築時間は最悪O(N log² N)。入力・出力はRustに保持し、IPC経路は追加しない。

## Instanceの空間階層

Rendererの`SpatialIndex`は登録asset instanceのworld AABBのみを保持する。asset内nodeとは別の二分treeで、source Instance slotをleafへ対応させる。boundsの長い軸で分割し、camera変更時に枝単位で不可視領域を除外する。frustum内候補だけが従来のlocal bounds／SSE判定へ進む。

dirty rangeに含まれるinstanceだけworld boundsを再計算し、leafから祖先へrefitする。camera変更ではrefitしない。membership／登録assetの構成変更時はtreeを再構築する。world boundsはf64で計算し、f32演算誤差の余裕と外向き丸めを加える。負scale、回転、非一様scale、shearに対応する。非有限値・overflow・非affine modelはbounds不明として枝刈りせず、従来の保守的判定へ渡す。

静止frameは階層走査もbounds判定も0。`CpuLodStats.spatial_nodes`は当frameの訪問node数、`bounds_tests`はleaf候補に対するlocal bounds／SSE判定数。現在はcamera変更時のcandidate metadata走査と、選択変更時のdraw range再構築がO(instance数)で残る。instance階層の訪問budgetや動的な再平衡も未実装であり、極端な移動後には枝刈り効率が落ちうる。asset cutの訪問budgetと混同しない。

## 互換性と検証

CPU選択はInstanceのGPU配置を変更しない。既存のGeometryBatches・差分転送・gather索引cache・primitive除外を維持する。THREEXA1、checkpoint／journal v7、request protocolは変更しない。多段clusterの永続化・配送はまだ接続していない。現行loaderは引き続きroot／detailを配送し、再送重複排除・古いjob結果拒否を既存テストで確認する。

CPUテスト：

- 3段treeの部分常駐・欠落child・訪問／draw budget・root欠落でのcoverage。
- 不正treeと上限、正確なcluster分割の決定性・親子range分割・入力index検証。
- 不可視1024 instanceをroot 1訪問で除外、leaf refit、未知bounds、membership再構築。
- 反転／shearのcorner包含とoverflow、256 instanceの更新をflat判定と比較して可視候補の脱落がないこと。
- 現行Renderer経路のhysteresis、root／detail混在、読み込み失敗fallback、camera変更時のInstance転送0・gather索引再送なし、可視領域へ戻るdirty instanceの再判定。

Rust workspaceのdebug／releaseは各204 passed（core 72、renderer 43、storage 71、Tauri host 18）。JS 13 passed、Clippy all-targets／all-features、format、diff checkも通過。

⭕️ [Pending] 実GPUでの描画・画像境界・popping・MacBook Air性能。native GPU pipeline testを実行したがMetal adapter NotFoundで開始できなかった。CPUテストはこれらの実機確認を代替しない。

MuTracerは共有geometry／instance階層の責務分離と背景準備の設計観点だけを参考にした。コード、shader、テスト、生成物は流用していない。Universal-3Dのworld boundsとshearの扱いを読み取り確認し、変換契約を維持する。どちらの参照ディレクトリもbuild・実行・テスト依存に含めない。

English: Asset-local coverage trees and world-space instance trees are independent. Atomic resident-child replacement retains parent fallback under budget pressure. An exact cluster partitioner preserves original vertices; boundary-preserving parent simplification is implemented. Multilevel page persistence/delivery and background generation remain G03-B2b. Sparse refits and existing instance/gather caches are preserved. GPU validation remains Pending.

简体中文：asset内部覆盖层级与instance世界空间层级独立实现。只有所有子page可用且预算允许时才替换父节点。精确cluster分割保留原顶点；已实现固定边界的父级简化；多层page保存／配送与后台生成仍属于G03-B2b。保留局部refit、差分传输和gather缓存，实机GPU验证保持Pending。
