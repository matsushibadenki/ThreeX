# Box／Plane／Sphere geometry

2026-10-17。🟢 [Done] 共有geometry・Plane／Sphereの任意分割・差分描画・永続化と復旧。
詳しいwire契約・移行とテストは [subdivisions.md](subdivisions.md) を参照。

| Geometry | 寸法と向き | 分割 |
| --- | --- | --- |
| BoxGeometry(width=1,height=1,depth=1) | 中心原点、単位Boxを各軸へ拡縮 | 従来の8 vertices／36 indices |
| PlaneGeometry(width=1,height=1,widthSegments=1,heightSegments=1) | XY面、法線+Z | 各軸1〜256 |
| SphereGeometry(radius=1,widthSegments=32,heightSegments=16) | Y軸を極とする単位球をradius倍 | 横3〜256、縦2〜256 |

寸法は有限・正・f32で表現可能な値。分割数は整数で、非有限・小数・範囲外は拒否する。
geometryと寸法・分割数はimmutable。`mesh.geometry = new SphereGeometry(2,48,24)` のように交換する。
交換はそのMeshだけをdirtyにし、同じgeometryオブジェクトの再設定は更新しない。
寸法をObject scaleへ組み込む既存Boxの契約を維持し、子にもそのscaleを継承する。
任意BufferGeometry、UV、normal、texture、部分球は未実装。既存のunlit RGB materialを使う。

```js
const ground = new Mesh(new PlaneGeometry(20, 20, 16, 16), material);
ground.rotation.x = -Math.PI / 2;
const ball = new Mesh(new SphereGeometry(2, 48, 24), material);
scene.add(ground, ball);
```

## Wire／Scene／描画

64-byte recordとopcodeを維持する。flagsのbit0はvisible、bit1はmesh、bits2–3はgeometry kind。
bits4–12と13–21はcustom分割数。既定のPlane 1×1／Sphere 32×16では両fieldを0へ正規化する。
Boxのflags・vertices／indicesと、既定形状のInstance tagは従来と同じ。
Object／SoAに分割属性を保持し、分割だけの変更はlocal／world行列を再計算しない。

80-byte Instanceのcolor.wへ分割を含む内部tagを保持する。f32で整数を正確に表現できる範囲を使い、
shader出力alphaは常に1。RGB・model・gatherのraw wordコピーは従来どおり。

Rust Rendererは3種類の既定meshと、参照中のcustom分割meshを共有する。
同じGeometrySpecはmeshを共有し、参照がなくなったcustom meshは次のScene更新で解放する。
頂点数が65,536以下ならUint16 index、それを超える最大分割等はUint32 indexを使う。

Instanceの転送は既存の3形状bankを維持する。dirty slotに対応する分割spec区間だけを更新する。
同じkind内の分割変更ではbankのsource／destination対応を変えず、gather計画・GPU索引を再利用する。
単一specは元bufferから直接描画する。単一kindでも複数specがある場合はbank順序で描画する。
既定の3形状だけなら最大3 drawsを維持し、custom specが混在する場合は連続spec区間ごとにdrawする。

🟠 [Next] custom spec別batching。交互spec配置はInstance数までdraw区間が増え得る。
active spec数に応じてGPU meshメモリも増える。resource budget／generation handlesも別の未完了項目。

分割gather・fallback・cacheの仕様は [partitioned-gather.md](partitioned-gather.md)、
[gather-cache.md](gather-cache.md)。これらのCPU性能値は任意分割導入前の測定で、現在のGPU性能の主張には使わない。

## 保存形式と復旧

🟢 [Done] checkpoint v6／journal THREEXJ6を出力し、v1〜5を読み込む。
旧journalはwriter開始時にv6へ原子的にre-anchorする。Document ID・request/session receipt・
Undo／Redoを維持し、再起動後の同一requestを重複実行しない。
未対応の旧binaryは新形式をversionエラーとして拒否する。

content hash schema v2を維持する。既存のcanonical flags u32に分割数も含まれ、既定形状のflags・
hash入力bytesは変わらない。v5以前と同じ既定内容のhashは同じで、異なる分割は異なるhashになる。
request protocol v1とipc-sessions v1も維持する。

以前のwriter lock修正も保持する。owner Dropで明示unlockし、重複descriptorの残存が再オープンを妨げず、
旧descriptorのcloseで新ownerのlockが外れないことを既存テストで確認する。

## 検証

Rust 152件（core 54、renderer 20、storage 61、Tauri host 17）とJS 13件。
分割属性のcanonical wire・tag精度・hash・checkpoint・Undo／Redo、差分行列再計算ゼロ、
最小／最大分割のtopology・u32 index、spec区間・gather索引再利用、v5移行・末尾切断／破損復旧・再送をテストする。

⭕️ [Pending] GPU pipeline testはcustom Planeと最大Sphereも対象に拡張したが、Metal adapterがNotFound。
実GPU・WASM／browser表示は未検証。CPU計画とWGSL validationを実機検証の代替とは扱わない。

English: Plane and Sphere support bounded integer subdivisions, shared meshes, incremental spec runs and
Uint16/Uint32 indexed draws. Default wire/tag/hash values remain compatible; storage v6 reads v1–5.
Custom interleaved specs can increase draw counts. GPU presentation remains pending.

简体中文：Plane与Sphere支持范围内的整数分段、共享mesh和差分绘制区间，按需使用Uint16／Uint32索引。
保留默认命令、tag和hash，保存格式v6可读取v1–5。交错的自定义spec可能增加draw数，实际GPU显示验证待完成。
