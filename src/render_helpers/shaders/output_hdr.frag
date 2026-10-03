// Convert the compositor's current SDR sRGB output into HDR10-style BT.2020/PQ signalling.
//
// The shader assumes SDR content is encoded as sRGB and maps SDR diffuse white to
// `sdr_white_nits`. Connector HDR metadata/colorimetry must only be enabled together with this
// transform.

uniform float sdr_white_nits;

vec3 srgb_to_linear(vec3 c) {
    bvec3 cutoff = lessThanEqual(c, vec3(0.04045));
    vec3 low = c / 12.92;
    vec3 high = pow((c + 0.055) / 1.055, vec3(2.4));
    return mix(high, low, vec3(cutoff));
}

vec3 rec709_to_rec2020(vec3 c) {
    return mat3(
        0.6274040, 0.0690970, 0.0163916,
        0.3292820, 0.9195400, 0.0880132,
        0.0433136, 0.0113612, 0.8955952
    ) * c;
}

vec3 linear_nits_to_pq(vec3 nits) {
    const float m1 = 0.1593017578125;
    const float m2 = 78.84375;
    const float c1 = 0.8359375;
    const float c2 = 18.8515625;
    const float c3 = 18.6875;

    vec3 l = clamp(nits / 10000.0, 0.0, 1.0);
    vec3 lm1 = pow(l, vec3(m1));
    return pow((vec3(c1) + c2 * lm1) / (vec3(1.0) + c3 * lm1), vec3(m2));
}

vec4 postprocess(vec4 color) {
    // The output framebuffer is expected to be opaque, but preserve alpha for correctness if this
    // element is reused for a non-opaque target later.
    vec3 linear709 = srgb_to_linear(clamp(color.rgb, 0.0, 1.0));
    vec3 linear2020 = max(rec709_to_rec2020(linear709), vec3(0.0));
    vec3 pq = linear_nits_to_pq(linear2020 * sdr_white_nits);
    return vec4(pq, color.a);
}
