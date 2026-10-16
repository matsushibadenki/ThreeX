struct Camera { vp: mat4x4<f32> }
@group(0) @binding(0) var<uniform> camera: Camera;
struct Out { @builtin(position) position: vec4<f32>, @location(0) color: vec4<f32> }
@vertex fn vs(@location(0) p: vec3<f32>, @location(1) a: vec4<f32>, @location(2) b: vec4<f32>, @location(3) c: vec4<f32>, @location(4) d: vec4<f32>, @location(5) color: vec4<f32>) -> Out {
 var out: Out; out.position = camera.vp * mat4x4<f32>(a,b,c,d) * vec4<f32>(p,1.0); out.color=vec4<f32>(color.rgb,1.0); return out;
}
@fragment fn fs(input: Out) -> @location(0) vec4<f32> { return input.color; }
