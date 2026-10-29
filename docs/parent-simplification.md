# G03-B2a：境界保持の親geometry簡略化

🟢 [Done] `threex_core::simplify::simplify`による境界保持のedge縮約と保守的error評価、`cluster_builder::SimplifiedClusters::build`による親geometry生成。

🟠 [Next] G03-B2b：多段page形式・loader・GPU接続、有界background builder／cache。今回の生成APIは明示呼び出しのoffline Rust APIで、Document worker／Rendererから自動実行しない。

## 使用と制限

`simplify`に元頂点、三角形index、追加の固定vertex ID、`SimplifyBudget`を渡す。結果は元vertex bufferを参照する三角形index、asset単位のerror、元vertexから最終endpointへの対応、候補評価回数と終了理由。頂点座標を書き換えず、新しいvertexも作らない。

`target_triangles`は目標の上限であり、必達ではない。`max_error`は有限・非負、`max_input_triangles`は入力triangle数の上限、`max_attempts`は候補縮約の評価回数の上限。境界・topology・向き・error制約により目標より多いtriangleを返すことがある。error上限0や評価回数0では元geometryを維持できる。非法index、非有限座標、空入力、無効設定はエラーとし、未完成の結果を公開しない。

入力patchの境界vertexをすべて固定する。clusterを跨ぐ境界、穴の縁、開いたmeshの輪郭も対象。追加locksはcreaseや属性seamを固定する用途。位置だけのAPIなのでUV／normal／materialの不連続は自動検出しない。同じ座標でも異なるvertex IDを溶接しない。

内部edgeのendpoint縮約候補を距離・vertex ID順に評価する。link条件、vertex linkの連結性、edgeの面数・向き、重複／退化face、元のfaceに対する向き反転、境界edge集合の変化を検証する。除去faceが写るedge／pointが出力meshに残ることも確認する。入力の非manifold edge、pinched vertex、不整合winding、重複／退化faceは`UnsupportedTopology`として元のgeometryをerror 0で返す。縮約途中で安全な候補がなくなった場合は最後に検証済みのgeometryを返し、一度も縮約していなければ元geometryを保持する。

これはendpoint縮約の保守的なCPU基準版で、quadric最適化や最良のtriangle削減率を保証しない。非隣接face同士の交差を調べるglobal collision検査も含まない。

## 面全体の誤差上限

元vertex `v`から最終endpoint `r(v)`への対応を保持し、`max ||v − r(v)||`を計算する。候補縮約ごとに、除去faceの像が保持meshのedge／pointに含まれることと、保持faceが元faceの像であることを検証する。この対応を合成したpiecewise-linear写像は元の表面全体から出力の表面全体への対応となる。

元triangle上の点をbarycentric係数λで表すと、対応点との距離は`||Σ λᵢ(vᵢ − r(vᵢ))|| ≤ Σ λᵢ ||vᵢ − r(vᵢ)|| ≤ max ||v − r(v)||`。出力triangleの全点にも元の点が対応するため、元→親・親→元の両方向のsurface distanceをこの値で上から抑える。単にvertexやfaceをサンプリングした最大値ではない。

距離はf64で計算し、差・hypotの丸めを覆う余裕と外向きのf32丸めを加える。有限f32のerrorで覆えない候補は採用しない。特に巨大座標でerrorがoverflowする場合は元geometryへ退く。平面の簡略化でもvertex対応の距離を使うため、実際のsurface distanceより大きなerrorを報告しうる。これは詳細LODへ早めに戻る保守的な評価。

## 階層への接続

`SimplifiedClusters::build`は既存のexact cluster分割を使い、各親の元triangle集合から直接簡略化する。簡略化済みの子geometryを次の親の入力にしないため、階層深さに応じた誤差の加算は不要。leafは元triangleを保持する。親と子の選択が混ざっても、それぞれのpatch境界edgeは元のまま保たれる。

出力は単一のsource vertex配列、nodeごとのmeshと対応表、coverage階層。nodeのerrorは自分のmesh errorと子node errorの最大値とし、階層上で単調な保守的envelopeにする。boundsは元のものを維持する。新vertexを生成しないため、親geometryもこのboundsに含まれる。

`ParentGeometryBudget`はsource vertex数、全nodeの保存triangle数、全体の候補評価回数を制限する。exact fallbackを保存できるtriangle容量を生成前に確認するため、簡略化後なら収まるケースでも保守的に容量不足を返すことがある。対応表・topologyの一時metadataもtriangle数に比例する。候補ごとにpatchのtopology等を再検証するため、大規模assetのrender threadで実行する用途には使わない。有界background化とcacheはG03-B2b。

## 互換性と検証

既存のcoverage treeの原子的な親→子置換、PagePoolのroot／proxy fallback、Document所有とstale job拒否は維持する。THREEXA1、checkpoint／journal v7、request protocolは変更しない。生成親を既存のroot、元meshをdetailとしてencode／decodeし、既存のCPU residency選択を通す回帰テストを追加した。多段pageの永続化・配送はまだ接続していない。

CPUテストで以下を確認する。

- 平面／曲面patchのtriangle削減、決定性、境界vertex・edgeと追加locksの保持。
- 元→親・親→元のface内部を含む独立した距離サンプリングが宣言error以下であること。
- 穴、閉じた形状、非manifold・不整合winding・重複／退化faceのfallback、極大／極小座標。
- node errorの単調性、leafのexact geometry、mixed LODの境界、部分常駐／訪問・draw予算不足時のcoverage。
- source／出力容量と全体の候補評価上限、error 0制約・候補評価0でのexact fallback。
- 生成rootの4→2 triangle化とpage roundtrip、detail読込失敗・退避・解放・容量0のfallback。
- 1200 instanceの交互配置で、page状態変更時のInstance転送0、1色編集時の転送1、gather索引再送なし。

検証結果：Rust workspaceはdebug／releaseとも204 passed（core 72、renderer 43、storage 71、Tauri host 18）。JS 13 passed。Clippy all-targets／all-features、format、diff checkも通過。

⭕️ [Pending] 実GPU画像の亀裂・popping、GPUでの生成親表示、MacBook Airの性能・品質確認。native GPU pipeline testはMetal adapter NotFoundで開始できなかった。CPUの境界検証や誤差上限テストと区別する。

Universal-3Dのworld bounds／shear契約を再確認し、Rust側所有と既存の変換・差分経路を維持した。MuTracerはasset／instance責務分離の設計参考に留め、コード・shader・テストを流用せず独立実装した。両参照ディレクトリへのbuild・実行・テスト依存は追加していない。

English: Boundary-locked endpoint contraction generates parent geometry with a conservative bidirectional surface-distance bound derived from a simplicial vertex correspondence. Each parent is generated directly from its original patch; hierarchy errors form a monotone envelope. Unsafe or constrained reductions retain exact geometry. Coverage, existing page fallback, sparse transfers and gather indices remain intact. Multilevel persistence/delivery and background generation remain G03-B2b; GPU validation is Pending.

简体中文：已独立实现固定边界的端点收缩和父级geometry生成。通过顶点对应的分片线性映射计算保守的双向表面距离上限；每个父级直接来自原始patch，层级error保持单调。无法安全简化时保留精确geometry。保留coverage、page fallback、局部传输和gather索引缓存。多层page保存／配送与后台生成仍属于G03-B2b，实机GPU验证保持Pending。
