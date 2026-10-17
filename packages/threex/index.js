import {cameraState, lookAt as cameraLookAt} from './camera.js';
let nextId = 1;
export class Vector3 {
 constructor(x=0,y=0,z=0,changed=()=>{}) {this._v=[x,y,z];this.changed=changed;}
 get x(){return this._v[0]} set x(v){this._set(0,v)}
 get y(){return this._v[1]} set y(v){this._set(1,v)}
 get z(){return this._v[2]} set z(v){this._set(2,v)}
 _set(i,v){if(!Number.isFinite(v))throw Error('Expected finite number');if(this._v[i]!==v){this._v[i]=v;this.changed();}}
 set(x,y,z){this.x=x;this.y=y;this.z=z;return this;}
}
export class Object3D {
 constructor(){if(nextId>0xffffffff)throw Error('Entity IDs exhausted');this.id=nextId++;this.children=[];this.parent=null;this._visible=true;this._disposed=false;const dirty=()=>this._mark();this.position=new Vector3(0,0,0,dirty);this.rotation=new Vector3(0,0,0,dirty);this.scale=new Vector3(1,1,1,dirty);}
 _root(){let n=this;while(n.parent)n=n.parent;return n;}
 _mark(){const r=this._root();if(r instanceof Scene)r._dirty.add(this);}
 get visible(){return this._visible}set visible(v){this._visible=!!v;this._mark();}
 add(...objects){for(const o of objects){if(!(o instanceof Object3D)||o._disposed||this._disposed)throw Error('Invalid object');for(let p=this;p;p=p.parent)if(p===o)throw Error('Hierarchy cycle');if(o.parent)o.parent.remove(o);o.parent=this;this.children.push(o);const r=this._root();if(r instanceof Scene)r._structure++;}return this;}
 remove(...objects){for(const o of objects){const i=this.children.indexOf(o);if(i>=0){const r=this._root();this.children.splice(i,1);o.parent=null;if(r instanceof Scene)r._structure++;}}return this;}
 dispose(){if(this.parent)this.parent.remove(this);this._disposed=true;}
}
export class Group extends Object3D {}
export class Scene extends Object3D {constructor(){super();this._dirty=new Set();this._structure=0;}}
function dimensions(values){if(values.some(x=>!Number.isFinite(x)||x<=0||!Number.isFinite(Math.fround(x))||Math.fround(x)===0))throw Error('Invalid geometry dimensions');return Object.freeze(values);}
export class BoxGeometry {constructor(width=1,height=1,depth=1){this.dimensions=dimensions([width,height,depth]);Object.freeze(this);}get kind(){return 0;}}
function segments(width,height,minWidth,minHeight){if(!Number.isInteger(width)||!Number.isInteger(height)||width<minWidth||height<minHeight||width>256||height>256)throw Error('Invalid geometry segments');return Object.freeze([width,height]);}
export class PlaneGeometry {constructor(width=1,height=1,widthSegments=1,heightSegments=1){this.segments=segments(widthSegments,heightSegments,1,1);this.dimensions=dimensions([width,height,1]);Object.freeze(this);}get kind(){return 1;}}
export class SphereGeometry {constructor(radius=1,widthSegments=32,heightSegments=16){this.segments=segments(widthSegments,heightSegments,3,2);this.dimensions=dimensions([radius,radius,radius]);Object.freeze(this);}get kind(){return 2;}}
function geometryFlags(g){if(g instanceof BoxGeometry)return 0;const [w,h]=g.segments;const defaults=g.kind===1?[1,1]:[32,16];return (g.kind<<2)|((w===defaults[0]&&h===defaults[1])?0:(w<<4)|(h<<13));}
function validGeometry(g){return g instanceof BoxGeometry||g instanceof PlaneGeometry||g instanceof SphereGeometry;}
export class MeshBasicMaterial {
 constructor({color=0xffffff}={}){this._listeners=new Set();this.color=color;}
 get color(){return this._color;}
 set color(value){if(!Number.isInteger(value)||value<0||value>0xffffff)throw Error('Invalid RGB color');if(value!==this._color){this._color=value;for(const listener of this._listeners)listener();}}
}
export class Mesh extends Object3D {
 constructor(geometry,material){super();this.geometry=geometry;this._materialChanged=()=>this._mark();this.material=material;}
 get geometry(){return this._geometry;}
 set geometry(value){if(this._disposed)throw Error('Disposed mesh');if(!validGeometry(value))throw Error('Expected Box, Plane or Sphere geometry');if(this._geometry!==value){this._geometry=value;this._mark();}}
 get material(){return this._material;}
 set material(value){if(this._disposed)throw Error('Disposed mesh');if(!(value instanceof MeshBasicMaterial))throw Error('Expected MeshBasicMaterial');if(this._material===value)return;this._material?._listeners.delete(this._materialChanged);this._material=value;value._listeners.add(this._materialChanged);this._mark();}
 dispose(){this._material?._listeners.delete(this._materialChanged);super.dispose();}
}
export class PerspectiveCamera extends Object3D {
 constructor(fov=75,aspect=1,near=.1,far=1000){super();Object.assign(this,{fov,aspect,near,far,zoom:1});this.up=new Vector3(0,1,0);this.updateProjectionMatrix();}
 updateProjectionMatrix(){cameraState(this);return this;}
 lookAt(x,y,z){return cameraLookAt(this,x,y,z);}
}
// One binary batch per render. IDs never recycle, preventing stale entity reuse.
export class CommandBridge {
 constructor(){this.known=new Map();this.scene=null;this.structure=-1;}
 encode(scene){if(!(scene instanceof Scene)||scene._disposed)throw Error('Expected live Scene');const records=[];const write=(op,o)=>{const b=new ArrayBuffer(64),v=new DataView(b);v.setUint32(0,op,true);v.setUint32(4,o.id,true);if(op===2){const dimensions=o instanceof Mesh?o.geometry.dimensions:[1,1,1];const color=o instanceof Mesh?o.material.color:0xffffff;if(!Number.isInteger(color)||color<0||color>0xffffff)throw Error('Invalid RGB color');const values=[...o.position._v,...o.rotation._v,...o.scale._v.map((x,i)=>x*dimensions[i]),((color>>16)&255)/255,((color>>8)&255)/255,(color&255)/255];values.forEach((x,i)=>v.setFloat32(8+i*4,x,true));v.setUint32(56,(o.visible?1:0)|(o instanceof Mesh?2|geometryFlags(o.geometry):0),true);v.setUint32(60,o.parent?.id??0,true);}records.push(new Uint8Array(b));};
 let active=this.known;const structural=this.scene!==scene||this.structure!==scene._structure;
 if(structural){active=new Map();const visit=o=>{active.set(o.id,o);for(const child of o.children)visit(child);};visit(scene);for(const [id,o]of this.known)if(!active.has(id))write(3,o);for(const [id,o]of active)if(!this.known.has(id))write(1,o);for(const o of active.values())write(2,o);}else{for(const o of scene._dirty)if(active.has(o.id))write(2,o);}
 const bytes=new Uint8Array(records.length*64);records.forEach((r,i)=>bytes.set(r,i*64));return {bytes,commit:()=>{this.known=active;this.scene=scene;this.structure=scene._structure;scene._dirty.clear();}};
 }
}
export class WebGPURenderer {
 constructor({canvas}={}){this.domElement=canvas??document.createElement('canvas');this.bridge=new CommandBridge();this.runtime=null;}
 async init(){const wasm=await import('./wasm/threex_wasm.js');await wasm.default();this.runtime=await wasm.Runtime.create(this.domElement);return this;}
 setSize(width,height){if(!Number.isFinite(width)||!Number.isFinite(height)||width<1||height<1)throw Error('Invalid viewport');this.domElement.width=Math.floor(width);this.domElement.height=Math.floor(height);}
 render(scene,camera){if(!this.runtime)throw Error('Call await renderer.init() first');if(!(camera instanceof PerspectiveCamera))throw Error('Expected PerspectiveCamera');const state=cameraState(camera);const batch=this.bridge.encode(scene);this.runtime.render_camera(batch.bytes,state.nodes,state.projection,this.domElement.width,this.domElement.height);batch.commit();}
 dispose(){this.runtime?.free();this.runtime=null;this.bridge=new CommandBridge();}
}
