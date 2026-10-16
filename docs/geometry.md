# Box／Plane／Sphere geometry

2026-10-16。🟢 [Done] Rustの共有geometry・混在描画・差分描画区間、JS primitive API、永続化と復旧。

## APIと形状

| Geometry | 寸法と向き | 分割 |
| --- | --- | --- |
| BoxGeometry(width=1,height=1,depth=1) | 中心原点、単位Boxを各軸へ拡縮 | 従来の8 vertices／36 indices |
| PlaneGeometry(width=1,height=1) | 中心原点、XY面、法線+Z | 4 vertices／6 indices、1×1のみ |
| SphereGeometry(radius=1) | 中心原点、Y軸を極とする単位球をradius倍 | 32×16、561 vertices／2880 indices |

寸法は有限・正・f32で表現可能な値。Plane／Sphereの未対応分割指定は黙って無視せず拒否する。
geometryはimmutableで、`mesh.geometry = new SphereGeometry(2)` のように交換する。
交換はそのMeshだけをdirtyにし、同じgeometryを再設定した場合は更新しない。
寸法をObject scaleへ組み込む既存Boxのwire契約を維持する。子へgeometry寸法のscaleも継承する点を含む。
任意BufferGeometry、UV、normal、texture、任意分割・部分球は未実装。既存のunlit RGB materialを使う。

```js
const box = new Mesh(new BoxGeometry(1, 2, 1), material);
const ground = new Mesh(new PlaneGeometry(20, 20), material);
ground.rotation.x = -Math.PI / 2;
const ball = new Mesh(new SphereGeometry(2), material);
scene.add(box, ground, ball);
renderer.render(scene, camera);
```

## Wire／Scene／GPU

64-byte recordとopcodeは維持する。flagsはbit0 visible、bit1 mesh、bits2–3 geometry。
geometry値はBox=0、Plane=1、Sphere=2。値3とその他の上位bitは拒否する。
Boxのflags・単位vertices／indices・80-byte Instanceのbyte配置は従来と同じ。
GeometryKindはObjectのauthoring属性とSoA列で保持する。

Instance.color.wは内部tag（Box=1、Plane=2、Sphere=3）として使用し、shader出力alphaは常に1。
RGBとmodelは従来どおり。geometryだけの変更はInstanceだけを更新し、local／world行列を再計算しない。
visible／mesh変更、swap-remove、再追加、Scene切替、履歴切れ時のfull-uploadも既存経路に従う。

Rendererは3種類のvertices／indicesを各device上で一度作り、Instance bufferとpipelineを共有する。
Instanceの並びを変えず、連続する同geometryの区間をdraw_indexedへ渡す。
区間境界はSceneのdirty rangeだけから更新し、フレームごとの全Instance比較・コピーを追加しない。
保持するのは1 byte/slotのgeometry tagと境界表。Transform／colorだけの変更では境界表を書き換えない。
Boxのみなら個数にかかわらず1 drawを維持する。

混在時のdraw数は連続区間数。形状が交互に並ぶ場合はInstance数まで増え得る。
🟠 [Next] geometry別の間接参照／batchingで、交互配置でもdraw数を形状数へ抑える。
この実装から混在100k sceneのGPU性能向上を主張しない。

## 保存形式と旧データ

🟢 [Done] checkpoint v5／journal THREEXJ5を出力し、v1–4を読み込む。
v4 journalはwriter起動時にv5へ原子的にre-anchorしてから新しい編集を受け付ける。
旧Boxのauthoring値・Document ID・request/sessionのreceiptは維持し、Undo／Redoも形状を復元する。
旧binaryが新geometryを破損した末尾と誤認して切り捨てないよう、保存形式のversionを上げた。
v5を未対応の旧readerはversionエラーで停止する。新形式を旧binaryで書き直さないこと。

意味内容hashはschema v2に更新し、geometry bitsを含める。Boxだけでもversionのdomainが変わるので
v1 hashとv2 hashを比較しない。交換・cacheにはCONTENT_HASH_VERSIONを必ず組み合わせる。
request protocolはv1、ipc-sessionsはv1のまま。編集＋receiptの原子的な永続化と再送重複排除は維持する。

復旧回帰で露見したwriter lockの終了経路も修正した。owner Drop時に明示unlockし、
fork-before-exec等で重複descriptorが一時的に残っても論理owner終了後の再オープンを妨げない。
新しいownerのlockが旧descriptorのcloseで外れないこともテストする。

## 検証

🟢 [Done] Rust追加6テスト：

- 3形状の非退化triangle・境界・外向きwinding、Sphereのunit radius。
- geometry区間の分割／結合／shrink／再成長とBox単独1 draw。
- 240回の形状・表示・mesh・移動・色・削除編集で、差分区間と全体参照結果の一致。
- shape-onlyの行列再計算ゼロ・1 dirty range、hash、checkpoint、Undo／Redo、無効kindの非適用。
- v4 Box移行、Plane→Sphereの末尾全切断位置・破損から正常prefixへ復旧、再起動再送と履歴。
- 重複descriptorを保持したowner解放とwriter lock再取得。

🟢 [Done] JS追加3テスト：Box wire互換、Plane／Sphereの寸法とflags、交換の1 record更新、
idle／hidden shape、寸法のimmutable性と不正入力・未対応分割の拒否。

workspace通常テストはRust 138件、JS 12件。Rust releaseも138件、Clippy・format検査を実施。

⭕️ [Pending] Box＋Plane＋SphereをdrawするGPU testは追加したが、Metal adapterがNotFoundで取得不可。
実GPU／browser／WASMビルドと表示は保留。WGSL validationとCPU描画計画の成功を実機表示とは扱わない。

English: Shared Box, XY Plane and UV Sphere geometry supports mixed indexed instancing and incremental
run updates, retaining the Box wire/Instance layout. Checkpoint/journal v5 migrates v1–4; content hash is v2.
Interleaved geometry can produce many draws; GPU presentation remains pending.

简体中文：共享Box、XY平面和球体支持混合索引实例绘制及区间差分更新，保留Box命令和Instance布局。
保存格式v5迁移v1–4，内容hash更新为v2。交错排列仍可能产生较多draw，实际GPU显示验证待完成。
