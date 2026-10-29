# MuTracerの設計参照方針

🟢 [Done] 配置されたMuTracerの概要、BVH、更新、progressive rendering、native Engine／Displayの設計資料を確認した。MuTracerはpath tracing、ThreeXはRust／wgpuを主体とする描画・巨大asset処理を担う。ここでは設計上の観点だけを記録する。

ユーザーの指定により、MuTracerのソース、shader、テスト、定数表、生成物、WASM、assetはコピー・翻訳移植・改変利用しない。今後の実装はThreeXの要件から独立して作成する。配置ディレクトリは一時的な読み取り専用の参考資料として扱い、import、Cargo path、include、ビルドscript、実行、テスト、配布に依存させない。この文書はディレクトリ削除後も読めるよう、参考点を自己完結して記述する。

## ThreeXで検討する設計

| 参考点 | ThreeXへの適用先 | 保持する制約 |
| --- | --- | --- |
| 共有geometryの加速構造とinstance側の階層を分離する | G03-Bのasset内cluster階層とinstance空間階層を別に設計する | レイ探索のBVHと、簡略化誤差・面のcoverageを必要とするLOD階層を同一視しない |
| transform、頂点、topologyの変更を区別する | G03-Bのbounds差分更新、G07の局所geometry編集 | transform変更でasset階層を再構築せず、topology変更時は対象asset revisionを更新する |
| 共有geometryの構築結果を再利用する | asset ID／revision／内容hashを用いるbuilder cache | builder形式・設定もkeyへ含め、異なる設定や古いrevisionの結果を誤用しない |
| 構築を背景処理に送り、完了まで前の有効な表示を保つ | G03-B builder、G04 streaming | Rust worker、メモリ・job数の上限、既存root／proxy fallback、古い非同期結果の拒否を維持する |
| Engine、出力ごとのRender、Displayの責務を分ける | 将来の複数view・render graph・品質設定 | Document／StateはRust所有、windowはview設定のみ。大容量geometry・画素はIPCに流さない |
| 操作中previewと静止後の高品質出力を使い分ける | G06の予算に基づく品質制御 | 既存on-demand schedulerを維持する。常時描画を導入せず、必要な処理だけを再開する |
| 表示設定と蓄積・計算結果の更新を区別する | 将来のtone mapping／post-processing | 表示だけの変更でSceneやgeometryを再転送しない。path tracingの導入は別途判断する |
| 固定条件の画素比較とCPU契約テストを区別する | G08の検証 | 相手の性能値をThreeXの実測値と扱わず、GPU未検証はPendingとする |

## 取り込まない前提

MuTracerの資料には、更新検出時に小さな値を比較する方式、大きなinstance配列の全copyや条件による全scene再packingも記載されている。ThreeXの既存dirty range、差分transaction、Instance bank、gather cacheを置き換える理由にはしない。

MuTracerのブラウザーWorker／同期fallback、TypeScript AOTとnative SDKのbuild構成も採用しない。ThreeXは既存のRust workerとwgpu経路を基準にし、高負荷な構築をUI／Document所有threadへ戻さない。OpenPBR、denoising、path tracingは今後の独立した検討候補であり、実装済みとは扱わない。

## 優先順位

- 🟢 [Done] 設計の参考点とコード非流用・一時配置の境界を記録。
- 🟠 [Next] G03-B：asset内LOD階層とinstance空間階層の責務、bounds更新、coverage、構築budgetをThreeX向けに独立設計・実装する。
- 🔴 [Later] G04／G06／G07で構築cache、streaming、表示品質、局所編集へ段階適用する。
- ⭕️ [Pending] MuTracerとの同条件の実GPU画質・性能比較。MuTracerの実行・ビルドや比較測定は今回行っていない。

参照した配置資料：README、docs/bvh.md、docs/updates.md、docs/progressive-rendering.md、native/README.md、native/docs/engine.md、native/docs/display.md。これは設計資料の読解記録であり、ソース流用やSDK接続の記録ではない。

English: MuTracer is a temporary, read-only architectural reference. No source, shaders, tests, generated artifacts or assets are reused, and no build/runtime/test dependency is added. ThreeX will independently evaluate separated asset/instance hierarchies, bounded background preparation, stale-result rejection and view/display ownership. G03-B remains the next priority; ray-tracing BVHs do not substitute for coverage-preserving LOD hierarchies.

简体中文：MuTracer仅作为临时、只读的架构参考，不复用源码、着色器、测试、生成物或资源，也不增加构建、运行或测试依赖。ThreeX将独立设计asset／instance层级、有界后台构建、过期结果拒绝和显示职责。G03-B仍为下一优先项；光线追踪BVH不能代替保持覆盖和简化误差约束的LOD层级。
