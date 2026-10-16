import {test} from 'node:test';
import assert from 'node:assert/strict';
import {PerspectiveCamera,Group,Scene,Vector3,WebGPURenderer} from '../../packages/threex/index.js';
import {cameraState} from '../../packages/threex/camera.js';
const close=(a,b)=>assert.ok(Math.abs(a-b)<1e-6,`${a} != ${b}`);
test('camera lookAt aligns -Z with world targets, including pole and coincident targets',()=>{
 const c=new PerspectiveCamera();c.lookAt(1,0,0);close(c.rotation.y,-Math.PI/2);
 c.lookAt(0,0,-1);c.rotation._v.forEach(v=>close(v,0));
 c.lookAt(0,1,0);close(c.rotation.x,Math.PI/2);assert.ok(c.rotation._v.every(Number.isFinite));
 c.lookAt(0,0,0);c.rotation._v.forEach(v=>close(v,0));
});
test('lookAt honors a rotated, translated and uniformly scaled parent',()=>{
 const p=new Group(),c=new PerspectiveCamera();p.position.set(10,2,3);p.rotation.y=Math.PI/2;p.scale.set(2,2,2);p.add(c);c.position.z=5;
 c.lookAt(new Vector3(10,2,3));c.rotation._v.forEach(v=>close(v,0));
 const packed=cameraState(c);assert.equal(packed.nodes.length,18);assert.deepEqual([...packed.nodes.slice(0,3)],[10,2,3]);assert.equal(packed.nodes[11],5);
});
test('lookAt rejects unsupported parent scale or invalid inputs before changing rotation',()=>{
 const p=new Group(),c=new PerspectiveCamera();p.add(c);c.rotation.set(.1,.2,.3);const before=[...c.rotation._v];
 p.scale.set(1,2,1);assert.throws(()=>c.lookAt(0,0,-1),/nonuniform/);assert.deepEqual(c.rotation._v,before);
 p.scale.set(1,1,1);assert.throws(()=>c.lookAt(NaN,0,0),/target/);c.up.set(0,0,0);assert.throws(()=>c.lookAt(0,0,-1),/direction/);assert.deepEqual(c.rotation._v,before);
});
test('camera projection and TRS packing reject invalid or rounded-singular states',()=>{
 assert.throws(()=>new PerspectiveCamera(180),/projection/);const c=new PerspectiveCamera();c.zoom=0;assert.throws(()=>c.updateProjectionMatrix(),/projection/);c.zoom=2;c.updateProjectionMatrix();
 const p=new Group();p.add(c);p.scale.x=0;assert.throws(()=>cameraState(c),/transform/);p.scale.x=1;c.near=1;c.far=1+1e-9;assert.throws(()=>cameraState(c),/projection/);
});
test('renderer sends camera state to Rust and preserves dirty edits when Rust rejects the camera',()=>{
 const scene=new Scene(),camera=new PerspectiveCamera(),parent=new Group();parent.add(camera);parent.rotation.y=.4;camera.lookAt(1,0,-2);
 const renderer=new WebGPURenderer({canvas:{width:64,height:48}});let calls=0;
 renderer.runtime={render_camera(commands,nodes,projection,w,h){calls++;assert.equal(nodes.length,18);assert.equal(projection.length,5);assert.equal(w,64);assert.equal(h,48);if(calls===1)throw Error('Singular camera');assert.ok(commands.length>0);}};
 assert.throws(()=>renderer.render(scene,camera),/Singular camera/);assert.equal(renderer.bridge.known.size,0);renderer.render(scene,camera);assert.equal(renderer.bridge.known.size,1);
});
