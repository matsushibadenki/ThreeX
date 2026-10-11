# Three.js互換・GPUネイティブWebレンダリングエンジン設計仕様書

仮称は `ThreeX` とします。

## 1. 目的

ThreeX は、Three.js互換の高レベルAPIを提供しながら、内部実装を Rust + WASM + WebGPU に置き換えた次世代Web 3Dレンダリングエンジンです。

主要目標は次の5点です。

1. Three.js利用者が低コストで移行できること
2. JavaScript側のCPUボトルネックを極力排除すること
3. GPU-driven rendering を中核に据えること
4. Apple Silicon、とくにMetal経由WebGPUで高効率に動作すること
5. 将来的にWeb以外のTauri / native Rustにも同じコアを流用できること

基本思想は、

```text
Three.js的API
    ↓
Thin JS Facade
    ↓
WASM Command Layer
    ↓
Rust ECS / Render Database
    ↓
GPU Scene
    ↓
WebGPU
    ↓
Metal / Vulkan / D3D12
```

です。

---

# 2. 非目標

初期段階では以下は狙いません。

Three.js全APIの完全互換、WebGL対応、古いブラウザ対応、CPU物理エンジン統合、完全なUnreal Engine級機能、ブラウザ外native版との100%同一APIです。

まずは「Three.jsの主要ユースケースの80%を高速に置き換える」ことを優先します。

---

# 3. 全体アーキテクチャ

ThreeXは6層構造にします。

```text
Layer 1
JavaScript Compatibility API

Layer 2
JS ↔ WASM Bridge

Layer 3
Rust Scene Runtime

Layer 4
Render World / GPU Scene

Layer 5
GPU Compute + Rendering

Layer 6
WebGPU Backend
```

より具体的には、

```text
JavaScript
 ├ Scene
 ├ Object3D
 ├ Mesh
 ├ Camera
 ├ Material
 └ Texture
      ↓
Command Buffer
      ↓
WASM
      ↓
Rust Runtime
 ├ ECS
 ├ Resource Manager
 ├ Transform System
 ├ Visibility System
 ├ Animation System
 └ Render Graph
      ↓
GPU Scene
 ├ Transform Buffer
 ├ Mesh Buffer
 ├ Material Buffer
 ├ Instance Buffer
 ├ Visibility Buffer
 └ Indirect Draw Buffer
      ↓
Compute
 ├ Frustum Culling
 ├ Occlusion Culling
 ├ LOD
 ├ Instance Compaction
 └ Draw Generation
      ↓
Render Pass
```

---

# 4. Three.js互換API

利用者側のコードは可能な限りThree.jsに寄せます。

```javascript
// /examples/basic-scene.js

import * as THREE from "threex";

const scene = new THREE.Scene();

const camera = new THREE.PerspectiveCamera(
  75,
  window.innerWidth / window.innerHeight,
  0.1,
  1000
);

const geometry = new THREE.BoxGeometry(1, 1, 1);

const material = new THREE.MeshStandardMaterial({
  color: 0x44aa88
});

const cube = new THREE.Mesh(
  geometry,
  material
);

scene.add(cube);

const renderer = new THREE.WebGPURenderer();

await renderer.init();

renderer.setSize(
  window.innerWidth,
  window.innerHeight
);

function animate() {
  cube.rotation.y += 0.01;

  renderer.render(scene, camera);

  requestAnimationFrame(animate);
}

animate();
```

見た目はほぼThree.jsですが、内部ではJSオブジェクトを直接描画しません。

各オブジェクトは作成時にRust側Entityへ対応付けます。

```text
JS Mesh
   ↓
EntityHandle: 1782
   ↓
Rust ECS
   ↓
GPU Instance Index: 354
```

---

# 5. JS側設計

JS側は極力薄くします。

最大の原則は、

「毎フレームWASM関数を数万回呼ばない」

です。

これはWASM化で最も失敗しやすいポイントです。

悪い設計は、

```text
10000 Mesh
×
position update
×
rotation update
×
visibility query
```

をJS→WASMで毎フレーム呼ぶ方式です。

代わりに、

```text
JS changes
↓
Dirty Queue
↓
Command Batch
↓
WASM once
```

とします。

例えば、

```javascript
cube.position.x = 10;
cube.rotation.y = 0.4;

mesh2.visible = false;
```

は即座にRustへ送らず、

```text
Dirty Object
1782
1820
```

として蓄積します。

`renderer.render()` 時に、

```text
UpdateTransform(1782)
SetVisible(1820,false)
```

をまとめて送ります。

---

# 6. Rustコア

Rust側は `wgpu` を中核にします。

基本モジュール構成は、

```text
/crates
 ├ core
 ├ scene
 ├ renderer
 ├ gpu-scene
 ├ material
 ├ geometry
 ├ texture
 ├ animation
 ├ culling
 ├ render-graph
 ├ shader
 ├ wasm-bindings
 └ platform
```

とします。

中核クレートは、

```text
threex-core
```

です。

---

# 7. ECS設計

Object3DをそのままRust structとして大量生成するのではなく、Entity Component Systemを採用します。

例えばMeshは、

```text
Entity 125
 ├ Transform
 ├ MeshRef
 ├ MaterialRef
 ├ Bounds
 ├ Visibility
 └ LayerMask
```

として表します。

重要なのはSoA化です。

例えばTransformを、

```rust
struct Transform {
    position: Vec3,
    rotation: Quat,
    scale: Vec3,
}
```

の配列として大量に持つより、

```text
positions[]
rotations[]
scales[]
```

として保持します。

概念的には、

```rust
// /crates/scene/src/transform_store.rs

pub struct TransformStore {
    pub positions: Vec<[f32; 4]>,
    pub rotations: Vec<[f32; 4]>,
    pub scales: Vec<[f32; 4]>,
    pub parents: Vec<u32>,
    pub dirty: Vec<u8>,
}
```

のようにします。

これによりSIMD処理しやすくなります。

---

# 8. GPU Scene

ThreeXの最重要部分です。

CPU上のSceneと、描画用Sceneを分離します。

```text
Logical Scene
     ↓
Render World
     ↓
GPU Scene
```

GPU Sceneは巨大なStorage Buffer群になります。

```text
GPUScene
 ├ ObjectBuffer
 ├ TransformBuffer
 ├ BoundingBuffer
 ├ MeshBuffer
 ├ MaterialBuffer
 ├ InstanceBuffer
 ├ LightBuffer
 ├ VisibilityBuffer
 └ DrawCommandBuffer
```

例えばObjectBufferは、

```wgsl
struct GPUObject {
    transform_index: u32,
    mesh_index: u32,
    material_index: u32,
    flags: u32,
}
```

のようなデータになります。

GPUから見ればJavaScriptオブジェクトは存在しません。

---

# 9. GPU-driven rendering

ThreeXの中核機能です。

通常のThree.jsは概念的に、

```text
for mesh in scene:
    if visible:
        renderer.draw(mesh)
```

ですが、ThreeXでは、

```text
CPU:
GPU Sceneを更新

GPU Compute:
全オブジェクトを走査
↓
可視判定
↓
LOD判定
↓
描画対象を圧縮
↓
Indirect Draw Command生成

GPU Render:
Indirect Draw
```

とします。

CPUは各Meshの描画命令を出しません。

---

# 10. Cullingパイプライン

初期版では3段階にします。

```text
Stage 1
Frustum Culling

Stage 2
LOD Selection

Stage 3
Occlusion Culling
```

Frustum CullingはCompute Shaderで行います。

```text
Object Bounding Sphere
      ↓
Camera Frustum
      ↓
visible / invisible
```

OcclusionはHi-Z方式を採用します。

```text
Depth Buffer
↓
Mip Pyramid
↓
Hi-Z
↓
Bounding Box Test
```

これにより大量オブジェクトをCPUへ戻す必要がありません。

---

# 11. Indirect Drawing

WebGPUで利用可能なindirect drawを使います。

基本は、

```text
drawIndexedIndirect()
```

です。

将来的にmulti-draw系機能が利用可能になれば積極利用します。

パイプラインは、

```text
Compute
↓
Indirect Arguments Buffer
↓
Render Pass
↓
drawIndexedIndirect
```

となります。

これによりJS側Draw Call数を大幅に減らします。

---

# 12. Geometry管理

Geometryは可能な限り巨大なBuffer Poolへまとめます。

従来は、

```text
Geometry A → VertexBuffer A
Geometry B → VertexBuffer B
Geometry C → VertexBuffer C
```

ですが、

```text
Global Vertex Buffer

AAAAA
BBBBBBBB
CCC
DDDDDDD
```

というArena方式を使います。

Index Bufferも同様です。

Geometryには、

```text
vertex_offset
index_offset
vertex_count
index_count
```

だけを持たせます。

これによりBuffer切り替えを削減します。

---

# 13. Material設計

Three.js互換性のために、

```text
MeshBasicMaterial
MeshStandardMaterial
MeshPhysicalMaterial
ShaderMaterial
```

などの高レベルAPIを用意します。

ただし内部ではMaterial Parameter Bufferへ変換します。

例えば、

```text
MaterialBuffer[52]

baseColor
roughness
metalness
emissive
normalScale
textureIndices
flags
```

です。

大量MaterialをStorage Bufferにまとめます。

---

# 14. Shaderシステム

Three.jsのTSL思想をさらに進めます。

独自のShader Intermediate Representationを設けます。

```text
Material Nodes
     ↓
ThreeX Shader IR
     ↓
Optimization
     ↓
WGSL
```

最適化は、

```text
Constant Folding
Dead Code Elimination
Common Subexpression Elimination
Node Deduplication
Binding Deduplication
Texture Sampling Deduplication
```

を行います。

将来的には、

```text
Shader IR
↓
WGSL

Shader IR
↓
Metal Shader Language
```

のようにnative backendにも転用できます。

---

# 15. Render Graph

描画順を固定コードにしないことが重要です。

Render Graphを使用します。

例えば、

```text
Depth Prepass
    ↓
Hi-Z Build
    ↓
GPU Culling
    ↓
Shadow Pass
    ↓
Opaque Pass
    ↓
Transparent Pass
    ↓
Post Process
```

を依存関係として表します。

RenderGraphが、

```text
resource lifetime
texture reuse
barrier
pass ordering
temporary buffer
```

を管理します。

---

# 16. Apple Silicon最適化

ここは専用方針を持たせます。

Apple SiliconはUnified Memoryなので、

```text
CPU RAM
+
GPU VRAM
```

が物理的に完全分離しているGPUとは異なります。

そのため、

「GPUへコピーする」

というより、

「CPUとGPUが共有メモリを効率的に扱う」

設計を重視します。

第一に、小さなBufferを大量生成しません。

```text
NG:

10000 objects
→ 10000 GPU buffers
```

ではなく、

```text
1 large storage buffer
```

を基本にします。

第二に、Apple GPUのtile-based renderingを意識します。

極力、

```text
large batches
few render passes
few attachment switches
```

にします。

不要なRenderPass分割は避けます。

第三に、中間Framebufferを減らします。

Apple GPUではTile Memoryを有効に使える構造が重要なので、

```text
GBuffer
↓
memory
↓
another pass
```

のような大量中間転送を避けます。

モバイル向けDeferred Renderingをそのまま移植するのではなく、Forward+ / Clustered Forwardを基本候補とします。

---

# 17. Apple向け推奨レンダリング方式

初期標準レンダラーは、

```text
Forward+
```

を推奨します。

理由は、

```text
Tile-based GPU
+
MSAA
+
memory bandwidth
```

との相性です。

構成は、

```text
Depth Prepass
↓
Cluster Build
↓
Light Assignment
↓
Opaque Forward
↓
Transparent
↓
Post Process
```

とします。

数百〜数千Lightを扱う場合、

```text
Screen
↓
3D Clusters
↓
Light Lists
```

をComputeで生成します。

---

# 18. Apple Silicon向けAdaptive Backend

GPUベンダーを検出してレンダリング設定を変えます。

```text
Apple GPU

Prefer:
large buffers
forward+
larger batches
reduced pass count
memory conservative mode
```

一方、

```text
NVIDIA / discrete GPU
```

では、

```text
higher bandwidth mode
more aggressive compute
larger intermediate buffers
```

を許容します。

ただしAPIレベルでは同じ挙動にします。

---

# 19. Transform更新

TransformはCPUとGPUを使い分けます。

小規模SceneではCPU。

```text
< 約5000 dynamic transforms

Rust SIMD
```

大規模SceneではGPU Computeを選択可能にします。

```text
> 約5000〜10000

GPU hierarchy update
```

ただし親子階層はGPUで扱いにくいため、

```text
hierarchy depth ordering
```

を事前計算します。

例えば、

```text
Level 0
Scene Root

Level 1
Parent Objects

Level 2
Children

Level 3
Grandchildren
```

単位でComputeを走らせます。

---

# 20. Static / Dynamic分離

全オブジェクトを同じ扱いにしません。

```text
Static
Semi-static
Dynamic
GPU-generated
```

の4種に分類します。

StaticはTransform Buffer更新すら行いません。

Semi-staticはDirty時だけ更新。

Dynamicは毎フレーム。

GPU-generatedはCPUへ戻しません。

この分類だけでもCPU負荷は大きく下げられます。

---

# 21. Animation

Skeleton AnimationもGPU化します。

従来、

```text
CPU bone calculation
↓
GPU
```

ではなく、

```text
Animation Data
↓
GPU Compute
↓
Bone Matrix Buffer
↓
Vertex Shader
```

にします。

数千キャラクターでもCPU負荷を抑える設計です。

---

# 22. Particle System

Particleは完全GPU管理を基本とします。

```text
Spawn
↓
Physics
↓
Lifetime
↓
Sorting
↓
Rendering
```

すべてComputeで処理します。

JS側には、

```javascript
particleSystem.emit({
  position,
  count: 1000
});
```

だけ提供します。

---

# 23. WASM設計

Rustは、

```text
wasm-bindgen
```

を利用します。

ただし、細かい関数を大量公開しません。

例えば、

```rust
set_position()
set_rotation()
set_scale()
```

を個別呼び出しするより、

```rust
apply_command_buffer(
    ptr: *const u8,
    len: usize
)
```

のような低レベルインターフェイスを中心にします。

JS側からバイナリCommandBufferを渡します。

---

# 24. Command Buffer形式

例えば、

```text
[Command Header]
opcode
entity
payloadOffset

[Payload]
```

です。

例:

```text
01 000123 px py pz
02 000123 qx qy qz qw
05 000542 0
```

を一括送信します。

これによりJS/WASM境界コストを抑えます。

---

# 25. SharedArrayBuffer対応

利用可能な環境では、

```text
SharedArrayBuffer
```

を使います。

理想構成は、

```text
Main Thread
    ↓
Shared Command Buffer

Worker
    ↓
WASM Runtime
    ↓
WebGPU
```

です。

レンダラーをWorkerへ移し、

```text
UI Thread
```

と分離します。

OffscreenCanvas対応ブラウザでは積極的に利用します。

---

# 26. Threading

WASM Threadsが使える場合、

```text
Main WASM Thread
 ├ Scene update
 ├ Asset parsing
 ├ Animation
 └ Render preparation
```

を分散します。

ただしGPU操作自体は単一Render Thread中心にします。

---

# 27. Asset Pipeline

対応形式は初期段階で、

```text
glTF 2.0
GLB
KTX2
Basis Universal
Draco
Meshopt
```

とします。

Three.jsのGLTFLoader互換APIを提供します。

```javascript
const loader = new THREE.GLTFLoader();

loader.load("scene.glb", (gltf) => {
  scene.add(gltf.scene);
});
```

内部処理はRustに移します。

---

# 28. Meshopt優先

Geometry compressionはMeshoptを標準推奨とします。

さらに、

```text
meshlets
```

へ変換可能にします。

将来、

```text
Geometry
↓
Meshlets
↓
GPU Culling
↓
Visible Meshlets
```

というNanite的構造へ拡張できます。

---

# 29. Virtual Geometry

v2以降の大きな機能として、

```text
Virtual Geometry
```

を導入します。

Geometryを、

```text
Mesh
↓
Clusters
↓
Meshlets
```

へ分解します。

画面上で必要な部分だけ描画します。

これは、

```text
Three.js版Nanite
```

に近いものです。

ただしWebGPUの制約に合わせて簡略化します。

---

# 30. Texture Streaming

大規模SceneではTexture Residency管理を入れます。

```text
Camera distance
Screen size
Priority
```

から必要Mipを決定します。

```text
4K texture
↓
遠距離
512 px mipのみ

近距離
↓
2K / 4K load
```

とします。

Apple SiliconではUnified Memoryとはいえ、メモリ帯域・総RAM共有のため有効です。

---

# 31. Memory Manager

独自GPU memory allocatorを持ちます。

```text
Buffer Arena
Texture Pool
Staging Pool
Temporary Resource Pool
```

を用意します。

毎フレーム、

```text
createBuffer
destroyBuffer
```

を繰り返すのは禁止です。

---

# 32. Resource Handle

JSからRustのメモリポインタを直接保持しません。

```text
ResourceID
```

方式にします。

例えば、

```text
GeometryHandle = u32
MaterialHandle = u32
TextureHandle = u32
EntityHandle = u32
```

です。

Generation IDも付け、

```text
index + generation
```

にするとuse-after-freeを防げます。

---

# 33. Garbage Collection

JS GCにGPU resource寿命を完全依存させません。

JS側Objectが破棄された場合、

```text
FinalizationRegistry
```

は補助として使えますが、明示的な

```javascript
mesh.dispose();
```

を推奨します。

Rust側はReference Count + Generation Handleで管理します。

---

# 34. WebGPU Backend

Rust側は `wgpu` を使うことで、

```text
Web
→ WebGPU

macOS
→ Metal

Windows
→ D3D12

Linux
→ Vulkan
```

への展開が容易になります。

ここが非常に重要です。

ThreeXはWeb用として始めますが、内部コアは、

```text
Browser Renderer

Native Renderer

Tauri Renderer
```

で共有可能にします。

---

# 35. Native版

将来的には、

```text
threex-web
threex-native
```

を分けます。

Web:

```text
JS
↓
WASM
↓
wgpu
```

Native:

```text
Rust application
↓
wgpu
```

となります。

macOSでは、

```text
wgpu
↓
Metal
```

なのでWASMオーバーヘッドも消えます。

---

# 36. Three.js互換レベル

互換性を3段階にします。

```text
Compatibility A
API syntax compatible

Compatibility B
Behavior compatible

Compatibility C
Rendering compatible
```

初期版はA+Bを優先します。

ピクセル単位でThree.jsと同じレンダリング結果にする必要はありません。

むしろ、

```text
same API
better renderer
```

を目標にします。

---

# 37. 対応API優先順位

Phase 1では、

```text
Scene
Object3D
Group

Mesh

PerspectiveCamera
OrthographicCamera

BufferGeometry

BoxGeometry
PlaneGeometry
SphereGeometry

MeshBasicMaterial
MeshStandardMaterial

Texture

AmbientLight
DirectionalLight
PointLight

WebGPURenderer

GLTFLoader
```

を実装します。

これだけでもThree.jsアプリのかなりの割合を動かせます。

---

# 38. デバッグモード

GPU-driven engineはデバッグが難しいため、可視化機能を最初から作ります。

例えば、

```text
renderer.debug.showBounds = true
renderer.debug.showClusters = true
renderer.debug.showCulling = true
renderer.debug.showLOD = true
renderer.debug.showOverdraw = true
```

です。

GPU timingも表示します。

```text
Frame
CPU 2.1 ms
GPU 5.3 ms

Culling 0.4 ms
Depth 0.8 ms
Lighting 1.7 ms
Opaque 1.9 ms
Post 0.5 ms
```

---

# 39. Performance HUD

標準で、

```text
FPS
CPU Frame Time
GPU Frame Time
Visible Objects
Culled Objects
Draw Commands
Triangles
VRAM estimate
Buffer usage
```

を表示可能にします。

これは開発上かなり重要です。

---

# 40. ベンチマーク

Three.jsとの差を常時計測します。

ベンチマークシーンは少なくとも、

```text
10k static meshes
100k static meshes

10k moving meshes

1M instances

100 lights
1000 lights

100 animated characters

large GLTF scene
```

を用意します。

Three.js WebGLRenderer、Three.js WebGPURenderer、ThreeXを比較します。

---

# 41. 最重要KPI

目標としては、

```text
10,000 objects

Three.js
CPU render preparation
5〜15ms

ThreeX
< 2ms
```

を狙います。

100,000 objectでは、

```text
CPU scalability
```

よりも、

```text
GPU throughput
```

が律速になる状態を理想とします。

---

# 42. Apple Siliconベンチマーク

最低限、

```text
M1
M2
M3
M4
```

世代を評価します。

特に、

```text
MacBook Air
```

を重要ターゲットにします。

ファンレス環境での持続性能も評価します。

単純な最大FPSだけではなく、

```text
FPS/W
```

つまり電力効率を重視します。

---

# 43. Adaptive Quality

Apple Siliconでは熱設計も考えます。

例えば、

```text
GPU frame > 16ms

LOD down
shadow resolution down
particle budget down
postprocess down
```

を自動調整可能にします。

モードとして、

```javascript
renderer.powerMode = "performance";
renderer.powerMode = "balanced";
renderer.powerMode = "efficiency";
```

を提供できます。

---

# 44. ファイル構成案

リポジトリは、

```text
/threex

/apps
  /examples
  /benchmark
  /viewer

/packages
  /threex
  /compat-three

/crates
  /core
  /scene
  /renderer
  /gpu-scene
  /render-graph
  /shader
  /geometry
  /material
  /texture
  /animation
  /culling
  /asset
  /wasm

/shaders
  /culling
  /lighting
  /materials
  /post

/tests
  /compat
  /render
  /performance
```

とするのが分かりやすいです。

---

# 45. 開発順序

最初からGPU-driven renderingを完全実装しない方がよいです。

第一段階では、

```text
Three.js API
↓
Rust Scene
↓
wgpu
↓
standard draw
```

を完成させます。

第二段階で、

```text
GPU Scene
```

へ移します。

第三段階で、

```text
Compute Culling
```

を導入。

第四段階で、

```text
Indirect Draw
```

を導入。

第五段階で、

```text
LOD
Hi-Z
```

を追加します。

この順序なら比較しやすく、どの改良が効いたかも測定できます。

---

# 46. MVP

最小実験版は非常に小さくできます。

MVPでは、

```text
Scene
Mesh
Camera
Transform
BoxGeometry
BasicMaterial

WebGPU renderer

100k cubes
```

だけ実装します。

比較するのは、

```text
Three.js WebGPURenderer

vs

Rust/WASM + WebGPU
```

です。

さらにThreeXで、

```text
CPU Driven

vs

GPU Driven
```

も比較します。

これだけでこのアーキテクチャの価値をかなり検証できます。

---

# 47. 最初の性能実験

最も重要な実験は、

```text
100,000 cubes
```

です。

4方式を比較します。

```text
A
Three.js Mesh ×100k

B
Three.js InstancedMesh

C
ThreeX CPU-driven

D
ThreeX GPU-driven
```

この比較で、

```text
Three.js APIを維持したまま
内部をGPU Scene化する意味
```

がかなり明確になります。

---

# 48. 重要な設計判断

このプロジェクトで最も重要なのは、「Rustだから速い」と考えないことです。

Rust/WASMだけでは、

```text
JavaScript
↓
Rust
↓
100000 draw calls
```

なら大して速くなりません。

本質的な改良は、

```text
CPU処理量そのものを減らす
```

ことです。

つまり、

```text
JavaScript → Rust
```

より重要なのは、

```text
CPU → GPU
```

です。

---

# 49. ThreeXの本質

このエンジンを一言で表すと、

```text
Three.js-compatible GPU Scene Engine
```

です。

Three.jsの外見を保ちつつ、

内部は、

```text
Object Oriented Scene Graph
```

ではなく、

```text
Data Oriented
+
GPU Driven
+
Compute Driven
```

にします。

---

# 50. 将来構想

最終的には、

```text
ThreeX

JS API
    ↓
Rust Core

 ├ WebGPU
 ├ Metal
 ├ Vulkan
 └ D3D12

        ↓

GPU Scene
 ├ Geometry
 ├ Animation
 ├ Lighting
 ├ Physics
 ├ Particles
 ├ Culling
 └ Rendering
```

という汎用3D基盤にできます。

さらに、

```text
ThreeX Web
ThreeX Native
ThreeX Tauri
```

を同じコアで実現できます。

ここまで行くと、単なるThree.js代替ではなく、

「Web技術で使える軽量GPU-nativeゲーム・CAD・可視化エンジン」

にかなり近づきます。