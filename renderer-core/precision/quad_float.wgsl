// Quad-single arithmetic adapted from the error-free transforms described by
// Hida, Li, and Bailey. Components are ordered from most to least significant.
alias Qf32 = vec4<f32>;

fn qf_from_f32(value: f32) -> Qf32 {
    return Qf32(value, 0.0, 0.0, 0.0);
}

fn qf_to_f32(value: Qf32) -> f32 {
    return value.x + value.y + value.z + value.w;
}

fn qf_quick_two_sum(a: f32, b: f32) -> vec2<f32> {
    let sum = a + b;
    return vec2<f32>(sum, b - (sum - a));
}

fn qf_two_sum(a: f32, b: f32) -> vec2<f32> {
    let sum = a + b;
    let b_virtual = sum - a;
    let a_virtual = sum - b_virtual;
    return vec2<f32>(sum, (a - a_virtual) + (b - b_virtual));
}

fn qf_two_product(a: f32, b: f32) -> vec2<f32> {
    let product = a * b;
    return vec2<f32>(product, fma(a, b, -product));
}

fn qf_three_sum(a_value: f32, b_value: f32, c_value: f32) -> vec3<f32> {
    let first = qf_two_sum(a_value, b_value);
    let second = qf_two_sum(c_value, first.x);
    let third = qf_two_sum(first.y, second.y);
    return vec3<f32>(second.x, third.x, third.y);
}

fn qf_three_sum_two(a_value: f32, b_value: f32, c_value: f32) -> vec2<f32> {
    let first = qf_two_sum(a_value, b_value);
    let second = qf_two_sum(c_value, first.x);
    return vec2<f32>(second.x, first.y + second.y);
}

fn qf_renormalize(c0_value: f32, c1_value: f32, c2_value: f32, c3_value: f32, c4_value: f32) -> Qf32 {
    var c0 = c0_value;
    var c1 = c1_value;
    var c2 = c2_value;
    var c3 = c3_value;
    var c4 = c4_value;

    var pair = qf_quick_two_sum(c3, c4);
    var carry = pair.x;
    c4 = pair.y;
    pair = qf_quick_two_sum(c2, carry);
    carry = pair.x;
    c3 = pair.y;
    pair = qf_quick_two_sum(c1, carry);
    carry = pair.x;
    c2 = pair.y;
    pair = qf_quick_two_sum(c0, carry);
    c0 = pair.x;
    c1 = pair.y;

    var s0 = c0;
    var s1 = c1;
    var s2 = 0.0;
    var s3 = 0.0;
    pair = qf_quick_two_sum(s0, s1);
    s0 = pair.x;
    s1 = pair.y;

    if s1 != 0.0 {
        pair = qf_quick_two_sum(s1, c2);
        s1 = pair.x;
        s2 = pair.y;
        if s2 != 0.0 {
            pair = qf_quick_two_sum(s2, c3);
            s2 = pair.x;
            s3 = pair.y;
            if s3 != 0.0 {
                s3 += c4;
            } else {
                s2 += c4;
            }
        } else {
            pair = qf_quick_two_sum(s1, c3);
            s1 = pair.x;
            s2 = pair.y;
            if s2 != 0.0 {
                pair = qf_quick_two_sum(s2, c4);
                s2 = pair.x;
                s3 = pair.y;
            } else {
                pair = qf_quick_two_sum(s1, c4);
                s1 = pair.x;
                s2 = pair.y;
            }
        }
    } else {
        pair = qf_quick_two_sum(s0, c2);
        s0 = pair.x;
        s1 = pair.y;
        if s1 != 0.0 {
            pair = qf_quick_two_sum(s1, c3);
            s1 = pair.x;
            s2 = pair.y;
            if s2 != 0.0 {
                pair = qf_quick_two_sum(s2, c4);
                s2 = pair.x;
                s3 = pair.y;
            } else {
                pair = qf_quick_two_sum(s1, c4);
                s1 = pair.x;
                s2 = pair.y;
            }
        } else {
            pair = qf_quick_two_sum(s0, c3);
            s0 = pair.x;
            s1 = pair.y;
            if s1 != 0.0 {
                pair = qf_quick_two_sum(s1, c4);
                s1 = pair.x;
                s2 = pair.y;
            } else {
                pair = qf_quick_two_sum(s0, c4);
                s0 = pair.x;
                s1 = pair.y;
            }
        }
    }
    return Qf32(s0, s1, s2, s3);
}

fn qf_add(a: Qf32, b: Qf32) -> Qf32 {
    var pair = qf_two_sum(a.x, b.x);
    let sum0 = pair.x;
    let roundoff0 = pair.y;
    pair = qf_two_sum(a.y, b.y);
    let sum1 = pair.x;
    let roundoff1 = pair.y;
    pair = qf_two_sum(a.z, b.z);
    let sum2 = pair.x;
    let roundoff2 = pair.y;
    pair = qf_two_sum(a.w, b.w);
    let sum3 = pair.x;
    let roundoff3 = pair.y;

    pair = qf_two_sum(sum1, roundoff0);
    let s1 = pair.x;
    var t0 = pair.y;
    var triple = qf_three_sum(sum2, t0, roundoff1);
    let s2 = triple.x;
    t0 = triple.y;
    let t1 = triple.z;
    pair = qf_three_sum_two(sum3, t0, roundoff2);
    let s3 = pair.x;
    t0 = pair.y + t1 + roundoff3;
    return qf_renormalize(sum0, s1, s2, s3, t0);
}

fn qf_negate(value: Qf32) -> Qf32 {
    return -value;
}

fn qf_subtract(a: Qf32, b: Qf32) -> Qf32 {
    return qf_add(a, qf_negate(b));
}

fn qf_multiply(a: Qf32, b: Qf32) -> Qf32 {
    var pair = qf_two_product(a.x, b.x);
    let p0 = pair.x;
    var q0 = pair.y;
    pair = qf_two_product(a.x, b.y);
    var p1 = pair.x;
    var q1 = pair.y;
    pair = qf_two_product(a.y, b.x);
    var p2 = pair.x;
    var q2 = pair.y;
    pair = qf_two_product(a.x, b.z);
    var p3 = pair.x;
    let q3 = pair.y;
    pair = qf_two_product(a.y, b.y);
    var p4 = pair.x;
    let q4 = pair.y;
    pair = qf_two_product(a.z, b.x);
    var p5 = pair.x;
    let q5 = pair.y;

    var triple = qf_three_sum(p1, p2, q0);
    p1 = triple.x;
    p2 = triple.y;
    q0 = triple.z;
    triple = qf_three_sum(p2, q1, q2);
    p2 = triple.x;
    q1 = triple.y;
    q2 = triple.z;
    triple = qf_three_sum(p3, p4, p5);
    p3 = triple.x;
    p4 = triple.y;
    p5 = triple.z;

    pair = qf_two_sum(p2, p3);
    let s0 = pair.x;
    var t0 = pair.y;
    pair = qf_two_sum(q1, p4);
    var s1 = pair.x;
    let t1 = pair.y;
    var s2 = q2 + p5;
    pair = qf_two_sum(s1, t0);
    s1 = pair.x;
    t0 = pair.y;
    s2 += t0 + t1;

    s1 += a.x * b.w + a.y * b.z + a.z * b.y + a.w * b.x + q0 + q3 + q4 + q5;
    return qf_renormalize(p0, p1, s0, s1, s2);
}

fn qf_divide(a: Qf32, b: Qf32) -> Qf32 {
    let q0 = a.x / b.x;
    var remainder = qf_subtract(a, qf_multiply(b, qf_from_f32(q0)));
    let q1 = remainder.x / b.x;
    remainder = qf_subtract(remainder, qf_multiply(b, qf_from_f32(q1)));
    let q2 = remainder.x / b.x;
    remainder = qf_subtract(remainder, qf_multiply(b, qf_from_f32(q2)));
    let q3 = remainder.x / b.x;
    remainder = qf_subtract(remainder, qf_multiply(b, qf_from_f32(q3)));
    let q4 = remainder.x / b.x;
    return qf_renormalize(q0, q1, q2, q3, q4);
}

fn qf_less(a: Qf32, b: Qf32) -> bool {
    if a.x != b.x { return a.x < b.x; }
    if a.y != b.y { return a.y < b.y; }
    if a.z != b.z { return a.z < b.z; }
    return a.w < b.w;
}
