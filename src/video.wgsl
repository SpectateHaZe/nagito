// Pass 0 (NV12 only): YUV -> RGB. Pass 1: source -> output size (FSR1 EASU, Catmull-Rom or bilinear).
// Pass 2: RCAS sharpen. EASU and RCAS are ports of AMD FidelityFX FSR 1 (MIT).

struct Params {
    src_size: vec2<f32>,
    upscale: u32,      // 0 = bilinear, 1 = Catmull-Rom, 2 = FSR1 EASU
    srgb_target: u32,  // 1 if the swapchain format is *Srgb and expects linear values
    sharpness: f32,    // 0..1, 0 = no sharpening
    // scalar pads keep the struct at 32 bytes; a vec3 here would align to 16 and grow it to 48
    yuv: u32,          // NV12 pass: bit 0 full range, bit 1 BT.601 (else BT.709)
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> p: Params;
@group(0) @binding(3) var tex2: texture_2d<f32>; // NV12 chroma, or the second input of a pass
@group(0) @binding(4) var tex3: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: VsOut;
    o.pos = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    o.uv = uv;
    return o;
}

// 9-tap Catmull-Rom using bilinear taps.
fn catmull_rom(uv: vec2<f32>) -> vec3<f32> {
    let pos = uv * p.src_size;
    let c1 = floor(pos - 0.5) + 0.5;
    let f = pos - c1;
    let w0 = f * (-0.5 + f * (1.0 - 0.5 * f));
    let w1 = 1.0 + f * f * (-2.5 + 1.5 * f);
    let w2 = f * (0.5 + f * (2.0 - 1.5 * f));
    let w3 = f * f * (-0.5 + 0.5 * f);
    let w12 = w1 + w2;
    let o12 = w2 / w12;
    let t0 = (c1 - 1.0) / p.src_size;
    let t3 = (c1 + 2.0) / p.src_size;
    let t12 = (c1 + o12) / p.src_size;
    var r = vec3<f32>(0.0);
    r += textureSampleLevel(tex, samp, vec2(t0.x, t0.y), 0.0).rgb * w0.x * w0.y;
    r += textureSampleLevel(tex, samp, vec2(t12.x, t0.y), 0.0).rgb * w12.x * w0.y;
    r += textureSampleLevel(tex, samp, vec2(t3.x, t0.y), 0.0).rgb * w3.x * w0.y;
    r += textureSampleLevel(tex, samp, vec2(t0.x, t12.y), 0.0).rgb * w0.x * w12.y;
    r += textureSampleLevel(tex, samp, vec2(t12.x, t12.y), 0.0).rgb * w12.x * w12.y;
    r += textureSampleLevel(tex, samp, vec2(t3.x, t12.y), 0.0).rgb * w3.x * w12.y;
    r += textureSampleLevel(tex, samp, vec2(t0.x, t3.y), 0.0).rgb * w0.x * w3.y;
    r += textureSampleLevel(tex, samp, vec2(t12.x, t3.y), 0.0).rgb * w12.x * w3.y;
    r += textureSampleLevel(tex, samp, vec2(t3.x, t3.y), 0.0).rgb * w3.x * w3.y;
    return clamp(r, vec3(0.0), vec3(1.0));
}

@fragment
fn fs_nv12(in: VsOut) -> @location(0) vec4<f32> {
    var y = textureSampleLevel(tex, samp, in.uv, 0.0).r;
    var c = textureSampleLevel(tex2, samp, in.uv, 0.0).rg - vec2(128.0 / 255.0);
    if ((p.yuv & 1u) == 0u) {
        y = (y - 16.0 / 255.0) * (255.0 / 219.0);
        c = c * (255.0 / 224.0);
    }
    var rgb: vec3<f32>;
    if ((p.yuv & 2u) != 0u) {
        rgb = vec3(y + 1.402 * c.y, y - 0.344136 * c.x - 0.714136 * c.y, y + 1.772 * c.x);
    } else {
        rgb = vec3(y + 1.5748 * c.y, y - 0.187324 * c.x - 0.468124 * c.y, y + 1.8556 * c.x);
    }
    return vec4(clamp(rgb, vec3(0.0), vec3(1.0)), 1.0);
}

fn luma(c: vec3<f32>) -> f32 {
    return c.b * 0.5 + c.r * 0.5 + c.g;
}

// One bilinear corner of EASU's edge analysis: A above, B C D across, E below C.
fn easu_set(dir: ptr<function, vec2<f32>>, len: ptr<function, f32>, w: f32, a: f32, b: f32, c: f32, d: f32, e: f32) {
    let dc = d - c;
    let cb = c - b;
    var lx = max(abs(dc), abs(cb));
    lx = 1.0 / max(lx, 1e-5);
    let dx = d - b;
    lx = clamp(abs(dx) * lx, 0.0, 1.0);
    lx *= lx;
    let ec = e - c;
    let ca = c - a;
    var ly = max(abs(ec), abs(ca));
    ly = 1.0 / max(ly, 1e-5);
    let dy = e - a;
    ly = clamp(abs(dy) * ly, 0.0, 1.0);
    ly *= ly;
    *dir += vec2(dx, dy) * w;
    *len += (lx + ly) * w;
}

// Lanczos-like lobe, stretched along the edge direction and clipped.
fn easu_tap(acc: ptr<function, vec4<f32>>, off: vec2<f32>, dir: vec2<f32>, len2: vec2<f32>, lob: f32, clp: f32, c: vec3<f32>) {
    var v = vec2(off.x * dir.x + off.y * dir.y, off.x * -dir.y + off.y * dir.x);
    v *= len2;
    let d2 = min(dot(v, v), clp);
    var wb = 0.4 * d2 - 1.0;
    var wa = lob * d2 - 1.0;
    wb *= wb;
    wa *= wa;
    wb = 1.5625 * wb - 0.5625;
    let w = wb * wa;
    *acc += vec4(c * w, w);
}

fn easu(uv: vec2<f32>) -> vec3<f32> {
    let pp0 = uv * p.src_size - 0.5;
    let fp = floor(pp0);
    let pp = pp0 - fp;
    let i = vec2<i32>(fp);
    let b = ld(i + vec2(0, -1)); let c = ld(i + vec2(1, -1));
    let e = ld(i + vec2(-1, 0)); let f = ld(i);              let g = ld(i + vec2(1, 0)); let h = ld(i + vec2(2, 0));
    let ii = ld(i + vec2(-1, 1)); let j = ld(i + vec2(0, 1)); let k = ld(i + vec2(1, 1)); let l = ld(i + vec2(2, 1));
    let n = ld(i + vec2(0, 2)); let o = ld(i + vec2(1, 2));
    let bL = luma(b); let cL = luma(c); let eL = luma(e); let fL = luma(f); let gL = luma(g); let hL = luma(h);
    let iL = luma(ii); let jL = luma(j); let kL = luma(k); let lL = luma(l); let nL = luma(n); let oL = luma(o);

    var dir = vec2(0.0);
    var len = 0.0;
    easu_set(&dir, &len, (1.0 - pp.x) * (1.0 - pp.y), bL, eL, fL, gL, jL);
    easu_set(&dir, &len, pp.x * (1.0 - pp.y), cL, fL, gL, hL, kL);
    easu_set(&dir, &len, (1.0 - pp.x) * pp.y, fL, iL, jL, kL, nL);
    easu_set(&dir, &len, pp.x * pp.y, gL, jL, kL, lL, oL);

    let dir2 = dot(dir, dir);
    if (dir2 < 1.0 / 32768.0) {
        dir = vec2(1.0, 0.0);
    } else {
        dir *= inverseSqrt(dir2);
    }
    len = len * 0.5;
    len *= len;
    let stretch = dot(dir, dir) / max(abs(dir.x), abs(dir.y));
    let len2 = vec2(1.0 + (stretch - 1.0) * len, 1.0 - 0.5 * len);
    let lob = 0.5 + ((1.0 / 4.0 - 0.04) - 0.5) * len;
    let clp = 1.0 / lob;

    var acc = vec4(0.0);
    easu_tap(&acc, vec2(0.0, -1.0) - pp, dir, len2, lob, clp, b);
    easu_tap(&acc, vec2(1.0, -1.0) - pp, dir, len2, lob, clp, c);
    easu_tap(&acc, vec2(-1.0, 1.0) - pp, dir, len2, lob, clp, ii);
    easu_tap(&acc, vec2(0.0, 1.0) - pp, dir, len2, lob, clp, j);
    easu_tap(&acc, vec2(0.0, 0.0) - pp, dir, len2, lob, clp, f);
    easu_tap(&acc, vec2(-1.0, 0.0) - pp, dir, len2, lob, clp, e);
    easu_tap(&acc, vec2(1.0, 1.0) - pp, dir, len2, lob, clp, k);
    easu_tap(&acc, vec2(2.0, 1.0) - pp, dir, len2, lob, clp, l);
    easu_tap(&acc, vec2(2.0, 0.0) - pp, dir, len2, lob, clp, h);
    easu_tap(&acc, vec2(1.0, 0.0) - pp, dir, len2, lob, clp, g);
    easu_tap(&acc, vec2(1.0, 2.0) - pp, dir, len2, lob, clp, o);
    easu_tap(&acc, vec2(0.0, 2.0) - pp, dir, len2, lob, clp, n);
    // Deringing: stay inside the 2x2 neighbourhood's range.
    let mn = min(min(f, g), min(j, k));
    let mx = max(max(f, g), max(j, k));
    return min(mx, max(mn, acc.rgb / acc.w));
}

@fragment
fn fs_upscale(in: VsOut) -> @location(0) vec4<f32> {
    let out_size = in.pos.xy / in.uv; // fragment position over uv recovers the target size
    // EASU only scales up; for same size or smaller, Catmull-Rom does the job.
    if (p.upscale == 2u && out_size.x > p.src_size.x + 0.5) {
        return vec4(easu(in.uv), 1.0);
    }
    if (p.upscale >= 1u) {
        return vec4(catmull_rom(in.uv), 1.0);
    }
    return vec4(textureSampleLevel(tex, samp, in.uv, 0.0).rgb, 1.0);
}

fn ld(c: vec2<i32>) -> vec3<f32> {
    let m = vec2<i32>(textureDimensions(tex)) - 1;
    return textureLoad(tex, clamp(c, vec2(0), m), 0).rgb;
}

@fragment
fn fs_rcas(in: VsOut) -> @location(0) vec4<f32> {
    let dim = vec2<f32>(textureDimensions(tex));
    let ip = vec2<i32>(in.uv * dim);
    let e = ld(ip);
    var col = e;
    if (p.sharpness > 0.0) {
        let b = ld(ip + vec2(0, -1));
        let d = ld(ip + vec2(-1, 0));
        let f = ld(ip + vec2(1, 0));
        let h = ld(ip + vec2(0, 1));
        let mn4 = min(min(b, d), min(f, h));
        let mx4 = max(max(b, d), max(f, h));
        let hit_min = mn4 / (4.0 * mx4 + 1e-5);
        let hit_max = (1.0 - mx4) / (4.0 * mn4 - 4.0 - 1e-5);
        let lobe3 = max(-hit_min, hit_max);
        let lobe = max(-0.1875, min(max(lobe3.r, max(lobe3.g, lobe3.b)), 0.0)) * p.sharpness;
        col = clamp((lobe * (b + d + f + h) + e) / (4.0 * lobe + 1.0), vec3(0.0), vec3(1.0));
    }
    if (p.srgb_target == 1u) {
        col = pow(col, vec3(2.2));
    }
    return vec4(col, 1.0);
}

// ---------- frame generation ----------
// Luma pyramid level, 8x8 block motion search, vector median, then a motion-compensated midpoint frame.

const LUMA_DIV: f32 = 4.0;
const SEARCH: i32 = 12; // +-12 quarter-res px = +-48 full-res px per source frame

@fragment
fn fs_luma(in: VsOut) -> @location(0) vec4<f32> {
    // Four bilinear taps cover the 4x4 source pixels under this quarter-res pixel.
    let px = 1.0 / vec2<f32>(textureDimensions(tex));
    var c = textureSampleLevel(tex, samp, in.uv + vec2(-px.x, -px.y), 0.0).rgb;
    c += textureSampleLevel(tex, samp, in.uv + vec2(px.x, -px.y), 0.0).rgb;
    c += textureSampleLevel(tex, samp, in.uv + vec2(-px.x, px.y), 0.0).rgb;
    c += textureSampleLevel(tex, samp, in.uv + vec2(px.x, px.y), 0.0).rgb;
    return vec4(dot(c * 0.25, vec3(0.299, 0.587, 0.114)), 0.0, 0.0, 1.0);
}

fn prev_l(c: vec2<i32>) -> f32 {
    let m = vec2<i32>(textureDimensions(tex)) - 1;
    return textureLoad(tex, clamp(c, vec2(0), m), 0).r;
}

fn cur_l(c: vec2<i32>) -> f32 {
    let m = vec2<i32>(textureDimensions(tex2)) - 1;
    return textureLoad(tex2, clamp(c, vec2(0), m), 0).r;
}

// Sum of absolute differences between the current block and the previous frame shifted by d.
fn sad(blk: ptr<function, array<f32, 64>>, origin: vec2<i32>, d: vec2<i32>) -> f32 {
    var s = 0.0;
    for (var i = 0; i < 64; i++) {
        s += abs((*blk)[i] - prev_l(origin + d + vec2(i % 8, i / 8)));
    }
    return s;
}

// Sub-pixel offset from a parabola through three costs.
fn refine(l: f32, c: f32, r: f32) -> f32 {
    let den = l - 2.0 * c + r;
    if (den <= 1e-5) {
        return 0.0;
    }
    return clamp(0.5 * (l - r) / den, -0.5, 0.5);
}

// Output: where each current-frame block came from in the previous frame, in quarter-res pixels.
@fragment
fn fs_me(in: VsOut) -> @location(0) vec4<f32> {
    let origin = vec2<i32>(in.pos.xy) * 8;
    var blk: array<f32, 64>;
    for (var i = 0; i < 64; i++) {
        blk[i] = cur_l(origin + vec2(i % 8, i / 8));
    }
    var best = vec2(0, 0);
    // Small bias toward no motion keeps flat areas and static HUD still.
    var best_cost = sad(&blk, origin, best) - 0.05;
    for (var dy = -SEARCH; dy <= SEARCH; dy++) {
        for (var dx = -SEARCH; dx <= SEARCH; dx++) {
            let d = vec2(dx, dy);
            let cost = sad(&blk, origin, d) + 0.004 * f32(abs(dx) + abs(dy));
            if (cost < best_cost) {
                best_cost = cost;
                best = d;
            }
        }
    }
    let c = sad(&blk, origin, best);
    let fx = refine(sad(&blk, origin, best - vec2(1, 0)), c, sad(&blk, origin, best + vec2(1, 0)));
    let fy = refine(sad(&blk, origin, best - vec2(0, 1)), c, sad(&blk, origin, best + vec2(0, 1)));
    return vec4(vec2<f32>(best) + vec2(fx, fy), 0.0, 1.0);
}

// Vector median of the 3x3 neighbourhood: drops lone wrong vectors.
@fragment
fn fs_mv_filter(in: VsOut) -> @location(0) vec4<f32> {
    let m = vec2<i32>(textureDimensions(tex)) - 1;
    let c = vec2<i32>(in.pos.xy);
    var v: array<vec2<f32>, 9>;
    for (var i = 0; i < 9; i++) {
        v[i] = textureLoad(tex, clamp(c + vec2(i % 3 - 1, i / 3 - 1), vec2(0), m), 0).rg;
    }
    var best = v[4];
    var best_d = 1e9;
    for (var i = 0; i < 9; i++) {
        var d = 0.0;
        for (var j = 0; j < 9; j++) {
            let e = abs(v[i] - v[j]);
            d += e.x + e.y;
        }
        if (d < best_d) {
            best_d = d;
            best = v[i];
        }
    }
    return vec4(best, 0.0, 1.0);
}

// tex = previous frame, tex2 = current frame, tex3 = motion field.
// Each pixel tries the motion of its 3x3 neighbouring blocks and keeps the one where both frames agree,
// so object edges follow the object instead of smearing the background.
@fragment
fn fs_interp(in: VsOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(tex));
    let bdims = vec2<i32>(textureDimensions(tex3));
    let block = vec2<i32>(in.uv * vec2<f32>(bdims));
    let luma_w = vec3(0.299, 0.587, 0.114);
    var best = vec3(0.0);
    var best_err = 1e9;
    for (var i = 0; i < 9; i++) {
        let bc = clamp(block + vec2(i % 3 - 1, i / 3 - 1), vec2(0), bdims - 1);
        let mv = textureLoad(tex3, bc, 0).rg * LUMA_DIV / dims; // in uv units
        let a = textureSampleLevel(tex, samp, in.uv + mv * 0.5, 0.0).rgb;
        let b = textureSampleLevel(tex2, samp, in.uv - mv * 0.5, 0.0).rgb;
        // the pixel's own block wins ties, so flat areas keep their measured motion
        let err = abs(dot(a - b, luma_w)) + select(0.004, 0.0, i == 4);
        if (err < best_err) {
            best_err = err;
            best = (a + b) * 0.5;
        }
    }
    // No vector fits (something appeared or vanished): fall back to the newer frame.
    let w = smoothstep(0.06, 0.2, best_err);
    let cur = textureSampleLevel(tex2, samp, in.uv, 0.0).rgb;
    return vec4(mix(best, cur, w), 1.0);
}
