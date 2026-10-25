# Plane／Sphere subdivisions

> 現行の保存形式はv7／hash v3。以下のv6／hash v2の記録は分割導入時点の仕様。[asset参照・移行](assets.md)。

2026-10-17。🟢 [Done] 任意の整数分割（範囲内）、共有mesh・差分draw、保存v6／旧形式読込。

## APIと範囲

```js
new PlaneGeometry(width = 1, height = 1, widthSegments = 1, heightSegments = 1);
new SphereGeometry(radius = 1, widthSegments = 32, heightSegments = 16);
mesh.geometry = new SphereGeometry(2, 48, 24);
```

Planeは各軸1〜256、Sphereは横3〜256・縦2〜256の整数を受け付ける。
小数、非有限値、範囲外は拒否する。Three.jsの暗黙のfloor／clampは行わない。
寸法は既存どおり有限・正・f32で表現可能な値。geometry・dimensions・segmentsはimmutable。
既定引数・明示的な既定分割のwire値は以前と同じ。
Boxは変更せず、UV／normal／texture／部分球は追加していない。

Rustの`Object.segments: [u16;2]`は`[0,0]`がkindごとの既定値。
`GeometrySpec::new(kind, segments)`で範囲を検証し、実際の分割数へ解決する。
Objectの寸法は従来どおりscaleで表現する。分割数を変更してもlocal／world行列は再計算しない。

## 64-byte wireと80-byte Instance

flagsを次のように拡張し、recordの長さ・opcode・他fieldの位置は変えない。

| bits | 内容 |
| --- | --- |
| 0 | visible |
| 1 | mesh |
| 2–3 | kind：Box=0、Plane=1、Sphere=2。3は拒否 |
| 4–12 | custom widthSegments、9 bits |
| 13–21 | custom heightSegments、9 bits |
| 22–31 | reserved、非zeroは拒否 |

既定Plane 1×1／Sphere 32×16は両segment fieldを0で表す。
custom時は両方を非zeroで格納する。既定値を非zeroで重複表現するrecordも拒否し、
同じ内容に別のcanonical flagsが生じないようにする。JSとObject.flags()は既定値を0へ正規化する。
Boxのsegment fieldは常に0。

Instance.color.wは`(GeometrySpec.flags() >> 2) + 1`。
既定Box／Plane／Sphereは1／2／3を維持し、custom値もf32で正確に表現できる整数範囲に収まる。
RGB・model・Instance stride=80 bytes・shaderのopaque alpha=1は維持する。
gatherは引き続き20個のu32をコピーし、tagを変換しない。

## Mesh生成と差分描画

PlaneはXY面・中心原点・法線+Z。gridの頂点数は`(w+1)(h+1)`、triangle数は`2wh`。
Sphereはunit radius=1、Y軸を極とし、同じ頂点数でtriangle数は`2w(h-1)`。
seam位置を一致させ、極の退化triangleを生成しない。外向きCCW windingを維持する。

既定のBox／Plane／Sphereの生成を保持する。頂点数65,536以下はUint16 index、それを超える場合はUint32。
256×256の頂点数は66,049、最大indexは66,048で、u16への切り詰めを行わない。
Planeの最大index数は393,216、Sphereは391,680。

RendererはGeometrySpecごとにmeshを共有し、参照がなくなったcustom meshを次のScene更新で解放する。
custom meshは[byte／件数budget](mesh-budget.md)で制限する。上限超過specは同kindの既定meshを使い、Rust側で理由を照会できる。
既存の3形状Instance bank・差分copy・gather・計画／GPU索引cacheは維持する。

bank内でGeometrySpecごとにInstance索引を連続配置し、active spec数だけdrawする。
通常のTransform／RGB編集はdirty slotのみ処理し、bank索引revisionを維持する。
追加・削除・kind／spec交換時は、同じspecの末尾と後続specの境界slotを局所交換する。
全bankのsortは行わない（[計測と契約](local-spec-packing.md)）。
移動したslotは差分転送し、索引が変わったbankのgather計画・GPU索引を再生成する。
単一specなら元Instance bufferから直接描画する。既定3形状だけなら最大3 draws。
[現在の実装・比較と制限](spec-batching.md)。

🟢 [Done] 構成編集時の局所再配置。
🟢 [Done] custom mesh resource budget。
🟠 [Next] generation handles。

## 保存・hash・旧binary

checkpoint v6／journal THREEXJ6を出力し、v1〜5を読み込む。
recordとcheckpointのfield配置はv5と同じで、segment flagsを新しく許可する。
v5以前のcheckpoint（historyを含む）ではsegment bitsを拒否する。
旧journalはwriter開始時にv6へ原子的にre-anchorしてから新しい編集を受け付ける。
未対応の旧binaryは新形式をversionエラーで拒否し、新しい分割編集を破損末尾と誤認して切り捨てない。

content hash schemaはv2のまま。既存のflags u32へ分割数を含め、canonical field・byte順・正規化は変えない。
有効なflagsの値域を拡張したもので、既定内容のhashは以前と同じ。
分割数が変わればflagsが変わりhashも変わる。
request protocol v1・ipc-sessions v1・永続Document ID・request/session receiptは維持する。
旧形式から移行しても、再送・Undo／Redo・保存／再起動で分割数を保持する。

## テスト

🟢 [Done] Rust debug／releaseとも152件成功：core 54、renderer 20、storage 61、Tauri host 17。
JS 13件、Clippy全target／feature、format、diff whitespace検査も成功。

追加5 Rustテストと1 JSテスト（既存JSの入力拒否も更新）で以下を検証する。

- canonical flags、上下限・不正値拒否、f32 tagの正確なroundtrip。
- 分割だけの編集が1 dirty range、local／world再計算ゼロ。hash・checkpoint・Undo／Redo、v5読込と不正v5分割拒否。
- 最小／非対称／既定／最大分割の頂点・index範囲・triangle数・非退化／外向きwinding、Sphereのradius、u32が必要なindex。
- 300回のspec交換、dirty draw区間、shrink、単一kind内の異なるspecとbank順序。索引変更時のgather cache無効化、通常編集時のcache維持。
- v5移行後のPlane 7×9→Sphere 256×256、末尾全切断位置・破損から最後の正常な編集へ復旧、再起動再送、Undo／Redo、保存／再起動。
- JSのconstructor・wire flags・immutable分割数、geometry交換の1 record・idleなし、不正分割の拒否、明示既定値の旧flags維持。

GPU pipeline testは既定形状に加え、custom Planeと最大Sphereをdrawするよう拡張した。

```sh
cargo test -p threex-renderer native_gpu_pipeline_and_draw -- --ignored --nocapture
```

⭕️ [Pending] 実行を試みたがMetal adapter NotFound。custom meshの実GPU表示・GPU時間、
WASM／browserのビルド・表示は未検証。CPU描画計画・topology・WGSL validationを実機検証の代替とは扱わない。

English: Plane and Sphere accept bounded integer subdivisions with immutable parameters. Shared spec meshes
and spec-grouped banks preserve incremental uploads with revision-aware gather caches; large meshes use Uint32 indices.
Storage v6 reads v1–5, while default wire/tag/hash values remain unchanged. GPU presentation is pending.

简体中文：Plane与Sphere支持范围内的整数分段和不可变参数。按spec共享mesh并分组绘制，
保留原有上传／gather缓存，大mesh使用Uint32索引。保存格式v6读取v1–5，默认命令／tag／hash不变。
实际GPU显示验证待完成。
