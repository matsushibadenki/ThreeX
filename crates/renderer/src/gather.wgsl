// Raw words preserve the 80-byte Instance layout, including all float bits.
struct Instance { words: array<u32, 20> }
@group(0) @binding(0) var<storage, read> source: array<Instance>;
@group(0) @binding(1) var<storage, read_write> destination: array<Instance>;
@group(0) @binding(2) var<storage, read> tasks: array<vec2<u32>>;

@compute @workgroup_size(64)
fn gather(@builtin(global_invocation_id) invocation: vec3<u32>) {
    let index = invocation.x;
    if index >= arrayLength(&tasks) { return; }
    let task = tasks[index];
    for (var word = 0u; word < 20u; word += 1u) {
        destination[task.y].words[word] = source[task.x].words[word];
    }
}
