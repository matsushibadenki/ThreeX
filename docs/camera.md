# Perspective camera

2026-10-16。🟢 [Done] camera回転・親TRS・zoom・world-space lookAtとRustの投影／view行列計算。
平行移動だけだった視点制約を解除し、既存Scene／Document形式と64-byte編集命令は変更しない。

## 計算と境界

`threex_core::Perspective { fov, aspect, near, far, zoom }.view_projection(nodes)` は
rootからcameraまでのTRS（position／XYZ Euler radians／scale、各9 floats）を受け取る。
既存Sceneと同じlocal行列を順に合成し、f64で逆行列を求めてf32のcolumn-major view-projectionを返す。
右手系、camera前方はlocal -Z、WebGPUのdepthはnear=0／far=1。
親の回転、非一様scale、反転や合成によるshearも、可逆で有限な行列の範囲でviewに反映する。
特異なTransform、非有限値、非正のnear／aspect／zoom、far<=near、fov範囲外、計算のoverflowを拒否する。

JS PerspectiveCameraはObject3Dのposition／rotation／scaleとparentを使い、zoomとupを追加した。
`lookAt(x,y,z)`／`lookAt(Vector3)` のtargetはworld座標。既定upはworld +Y。
一致するeye／targetやupと視線の平行時には決定的な姿勢を選び、NaNを作らない。
lookAtは親の実効行列が正の向きの一様scale＋回転の場合に対応し、非一様scale／shear／反転を拒否する。
camera自身のscaleはlookAt時には正値に限定する。通常のview計算では非一様・負scaleも扱える。
無効なtarget／up／親条件はrotationの変更前に検査する。

```js
const camera = new PerspectiveCamera(60, width / height, 0.1, 2000);
camera.position.set(4, 3, 8);
camera.zoom = 1.25;
camera.lookAt(0, 0, 0);
camera.updateProjectionMatrix();
renderer.render(scene, camera);
```

updateProjectionMatrixは設定を検証する。実際の行列生成はrender時のRust側で行うため、
パラメータ変更が古いJS側projection cacheに残らない。film offset／view offset／物理camera APIや
JSのMatrix4／Quaternion互換APIはこの実装範囲に含めない。

## Runtime接続

WebGPURendererはroot-to-cameraの小さいTRS列と投影設定5 floatsをRuntime.render_cameraへ渡す。
Rustでカメラを検証・計算してから既存render／Scene適用へ進む。カメラ拒否時にはSceneを変更せず、
JS CommandBridgeもcommitしないので、修正後に未送信編集を再試行できる。
frame pixelやInstance配列はJSへ出さず、Scene・Renderer・GPU bufferは引き続きRust側に置く。
カメラ状態の計算量と転送量は祖先数に比例し、Scene全体を走査・複製しない。

従来のRuntime.render(commands, vp, ...)は低水準APIとして保持する。
JS packageと生成済みWASM bindingは対応させて再ビルドすること。古いbindingにはrender_cameraがない。
Tauri native側でも同じRust Perspective計算を使って既存RedrawHandle／Drawへ行列を渡せる。
この変更は視点計算の追加であり、永続Documentへprojection設定を保存したりrequest/sessionを変更したりしない。

## 検証と保留

🟢 [Done] Rust追加3テスト：near／far depth、aspect／zoom／translation、回転・非一様scaleの親子と
world点の再投影、無効設定・特異行列の拒否。coreは51件をdebug／releaseで検証。

🟢 [Done] JS追加5テスト：world lookAt、極／一致target、親の回転・一様scale、無効状態の非変更、
f32丸めでのfar=near拒否、Rustへの状態送信、Rust拒否後のdirty編集再試行。
全workspaceのDocument・再送・native scheduler回帰テストも実行する。

⭕️ [Pending] wasm32 targetとwasm-packがないため、新render_camera bindingのWASMビルドとbrowser表示。
実GPU／nativeの視点描画・クリッピング確認も保留。CPUテストを実機描画の成功として扱わない。

🟢 [Done] Plane／Sphere geometryと対応描画：[geometry](geometry.md)。
🟠 [Next] cameraのview offset／film設定などの互換API拡張。

English: Camera rotation, parent transforms, zoom and world-space lookAt are implemented.
Rust computes the full affine view inverse and WebGPU projection; JS sends camera authoring state only.
LookAt rejects nonuniform/reflected parent transforms. Browser/WASM rendering verification remains pending.

简体中文：已实现相机旋转、父级Transform、缩放和世界坐标lookAt。Rust计算完整仿射逆矩阵及WebGPU投影，
JS仅传递相机状态。lookAt不支持父级非均匀缩放或反射。浏览器／WASM绘制验证仍待完成。
