// Camera authoring only. Projection and affine view inversion run in Rust.
const finite32 = v => Number.isFinite(v) && Number.isFinite(Math.fround(v));
export function cameraState(camera) {
 const projection=[camera.fov,camera.aspect,camera.near,camera.far,camera.zoom];
 if(!projection.every(finite32)||!(camera.fov>0&&camera.fov<180&&camera.aspect>0&&camera.near>0&&camera.far>camera.near&&camera.zoom>0))throw Error('Invalid camera projection');
 const chain=[],seen=new Set();for(let n=camera;n;n=n.parent){if(seen.has(n)||n._disposed)throw Error('Invalid camera hierarchy');seen.add(n);chain.push(n);}
 const nodes=[];for(const n of chain.reverse()){const pose=[...n.position._v,...n.rotation._v,...n.scale._v];if(!pose.every(finite32)||pose.slice(6).some(v=>Math.fround(v)===0))throw Error('Invalid camera transform');nodes.push(...pose);}
 const packed=new Float32Array(projection);if(!(packed[0]>0&&packed[0]<180&&packed[1]>0&&packed[2]>0&&packed[3]>packed[2]&&packed[4]>0))throw Error('Invalid camera projection');
 return {nodes:new Float32Array(nodes),projection:packed};
}
const identity=()=>[1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1];
function multiply(a,b){const m=new Array(16).fill(0);for(let c=0;c<4;c++)for(let r=0;r<4;r++)for(let k=0;k<4;k++)m[c*4+r]+=a[k*4+r]*b[c*4+k];return m;}
function local(n){const [x,y,z]=n.rotation._v,[sx,cx]=[Math.sin(x),Math.cos(x)],[sy,cy]=[Math.sin(y),Math.cos(y)],[sz,cz]=[Math.sin(z),Math.cos(z)];const m=[cy*cz,sx*sy*cz+cx*sz,-cx*sy*cz+sx*sz,0,-cy*sz,-sx*sy*sz+cx*cz,cx*sy*sz+sx*cz,0,sy,-sx*cy,cx*cy,0,...n.position._v,1];for(let c=0;c<3;c++)for(let r=0;r<3;r++)m[c*4+r]*=n.scale._v[c];return m;}
const dot=(a,b)=>a.reduce((v,x,i)=>v+x*b[i],0);
const cross=(a,b)=>[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]];
function unit(v){const length=Math.hypot(...v);if(!Number.isFinite(length)||length===0)throw Error('Invalid camera direction');return v.map(x=>x/length);}
export function lookAt(camera,x,y,z){
 const target=typeof x==='object'&&x!==null?[x.x,x.y,x.z]:[x,y,z];
 if(!target.every(finite32))throw Error('Invalid camera target');cameraState(camera);if(camera.scale._v.some(v=>v<=0))throw Error('lookAt requires positive camera scale');
 const chain=[];for(let p=camera.parent;p;p=p.parent)chain.push(p);let parent=identity();for(const n of chain.reverse())parent=multiply(parent,local(n));
 const columns=[0,1,2].map(c=>parent.slice(c*4,c*4+3)),lengths=columns.map(c=>Math.hypot(...c));
 const basis=columns.map(unit),scale=Math.max(...lengths);
 if(lengths.some(v=>Math.abs(v-lengths[0])>scale*1e-6)||Math.abs(dot(basis[0],basis[1]))>1e-6||Math.abs(dot(basis[0],basis[2]))>1e-6||Math.abs(dot(basis[1],basis[2]))>1e-6||dot(cross(basis[0],basis[1]),basis[2])<0)throw Error('lookAt requires a parent without nonuniform scale or reflection');
 const position=parent.slice(12,15).map((v,r)=>v+camera.position._v.reduce((s,p,c)=>s+parent[c*4+r]*p,0));
 let back=position.map((v,i)=>v-target[i]);if(Math.hypot(...back)===0)back=[0,0,1];back=unit(back);
 let up=unit(camera.up._v),right=cross(up,back);if(Math.hypot(...right)<1e-10){up=Math.abs(back[1])<0.9?[0,1,0]:[0,0,1];right=cross(up,back);}right=unit(right);up=cross(back,right);
 const rotation=[right,up,back].flatMap(column=>basis.map(axis=>dot(axis,column)));
 const ry=Math.asin(Math.max(-1,Math.min(1,rotation[6])));
 const rx=Math.abs(rotation[6])<0.9999999?Math.atan2(-rotation[7],rotation[8]):Math.atan2(rotation[5],rotation[4]);
 const rz=Math.abs(rotation[6])<0.9999999?Math.atan2(-rotation[3],rotation[0]):0;
 camera.rotation.set(rx,ry,rz);return camera;
}
