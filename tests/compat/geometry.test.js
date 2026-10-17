import {test} from 'node:test';import assert from 'node:assert/strict';
import {Scene,Mesh,BoxGeometry,PlaneGeometry,SphereGeometry,MeshBasicMaterial,CommandBridge} from '../../packages/threex/index.js';
const record=(batch,index)=>new DataView(batch.bytes.buffer,batch.bytes.byteOffset+64*index,64);
test('Box wire bytes are unchanged and Plane/Sphere dimensions and flags are explicit',()=>{
 const scene=new Scene(),box=new Mesh(new BoxGeometry(2,3,4),new MeshBasicMaterial()),plane=new Mesh(new PlaneGeometry(5,6),new MeshBasicMaterial()),sphere=new Mesh(new SphereGeometry(2),new MeshBasicMaterial());scene.add(box,plane,sphere);const b=new CommandBridge().encode(scene);
 const updates=[];for(let i=0;i<b.bytes.length/64;i++){const r=record(b,i);if(r.getUint32(0,true)===2&&r.getUint32(4,true)!==scene.id)updates.push(r);}
 assert.deepEqual(updates.map(r=>r.getUint32(56,true)),[3,7,11]);assert.deepEqual(updates.map(r=>[32,36,40].map(n=>r.getFloat32(n,true))),[[2,3,4],[5,6,1],[2,2,2]]);
});
test('geometry replacement is one dirty record, idle is empty, and hidden shape is retained',()=>{
 const s=new Scene(),a=new Mesh(new BoxGeometry(),new MeshBasicMaterial()),b=new Mesh(new BoxGeometry(),new MeshBasicMaterial());s.add(a,b);const bridge=new CommandBridge();bridge.encode(s).commit();a.geometry=new PlaneGeometry();let batch=bridge.encode(s);assert.equal(batch.bytes.length,64);assert.equal(record(batch,0).getUint32(56,true),7);batch.commit();assert.equal(bridge.encode(s).bytes.length,0);
 a.visible=false;a.geometry=new SphereGeometry();batch=bridge.encode(s);assert.equal(record(batch,0).getUint32(56,true),10);batch.commit();a.visible=true;assert.equal(record(bridge.encode(s),0).getUint32(56,true),11);assert.throws(()=>a.geometry={},/geometry/);
});
test('geometry dimensions and segments are immutable and invalid tessellation is rejected',()=>{
 assert.throws(()=>new PlaneGeometry(0),/dimensions/);assert.throws(()=>new SphereGeometry(NaN),/dimensions/);assert.throws(()=>new SphereGeometry(1,2,4),/segments/);assert.throws(()=>new PlaneGeometry(1,1,257,2),/segments/);assert.throws(()=>new BoxGeometry(Number.MAX_VALUE),/dimensions/);
 const g=new SphereGeometry();assert.throws(()=>g.dimensions[0]=2,TypeError);
});

test('custom segments use one dirty record and canonical defaults keep old flags',()=>{
 const s=new Scene(),m=new Mesh(new PlaneGeometry(),new MeshBasicMaterial());s.add(m);const bridge=new CommandBridge();bridge.encode(s).commit();
 for(const [geometry,w,h,kind] of [[new PlaneGeometry(2,3,7,9),7,9,1],[new SphereGeometry(2,8,4),8,4,2],[new SphereGeometry(1,256,256),256,256,2]]){
  m.geometry=geometry;const b=bridge.encode(s);assert.equal(b.bytes.length,64);assert.equal(record(b,0).getUint32(56,true),3|(kind<<2)|(w<<4)|(h<<13));b.commit();assert.equal(bridge.encode(s).bytes.length,0);assert.throws(()=>geometry.segments[0]=3,TypeError);
 }
 m.geometry=new SphereGeometry(1,32,16);assert.equal(record(bridge.encode(s),0).getUint32(56,true),11);
 for(const v of [0,-1,1.5,NaN,Infinity,257])assert.throws(()=>new PlaneGeometry(1,1,v,2),/segments/);
 assert.throws(()=>new SphereGeometry(1,3,1),/segments/);
});
