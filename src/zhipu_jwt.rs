//! 智谱 v4 端点 JWT 鉴权：`{id}.{secret}` → HS256 JWT（`sign_type=SIGN`）。
//!
//! 背景：open.bigmodel.cn 的 `/api/paas/v4/*` 家族端点对「Authorization:
//! Bearer 原始 API Key」的支持不稳定（不少账号/端点直接 401「令牌已过期或
//! 验证不正确」），而 JWT 方式（官方 develop 文档的「JWT Token 鉴权」）
//! 一直受支持。网关对 `provider=zhipu` 且 Key 形如 `{id}.{secret}` 的渠道
//! 自动换 JWT；其余 provider / Key 格式仍走原始 Bearer（硅基流动等
//! OpenAI 兼容源不需要）。
//!
//! JWT 规范（智谱官方文档 + 多语言 SDK 一致）：
//!   header  = {"alg":"HS256","sign_type":"SIGN"}
//!   payload = {"api_key":{id}, "exp":now_ms+ttl_ms, "timestamp":now_ms}
//!   sig     = base64url(HMAC-SHA256(secret, header_b64 + "." + payload_b64))
//!
//! 零依赖手写 SHA-256 + HMAC-SHA256 + base64url（wasm32 友好，不为一个
//! 百行算法拉入 ring/hmac 重型依赖）；正确性由标准测试向量保证（见本文件
//! 尾部 #[cfg(test)]）。

use serde::Serialize;

/// Key 是否为智谱 `{id}.{secret}` 两段式（ cheap probe，决定 raw/JWT 鉴权）。
pub fn looks_like_zhipu_key(key: &str) -> bool {
    match key.split_once('.') {
        Some((id, secret)) => !id.is_empty() && !secret.is_empty(),
        None => false,
    }
}

/// 生成智谱 JWT。`api_key` 必须是 `{id}.{secret}`；`now_ms` 由调用方给
/// （JS `Date.now()`，毫秒 epoch）；`ttl_ms` 有效期，官方推荐 1 小时。
pub fn sign_zhipu_jwt(api_key: &str, now_ms: f64, ttl_ms: f64) -> Option<String> {
    let (id, secret) = api_key.split_once('.')?;
    if id.is_empty() || secret.is_empty() {
        return None;
    }

    #[derive(Serialize)]
    struct Header<'a> {
        alg: &'a str,
        sign_type: &'a str,
    }
    #[derive(Serialize)]
    struct Payload<'a> {
        api_key: &'a str,
        exp: i64,
        timestamp: i64,
    }

    let header = Header {
        alg: "HS256",
        sign_type: "SIGN",
    };
    let payload = Payload {
        api_key: id,
        exp: (now_ms + ttl_ms) as i64,
        timestamp: now_ms as i64,
    };

    let header_b64 = base64url(serde_json::to_string(&header).ok()?.as_bytes());
    let payload_b64 = base64url(serde_json::to_string(&payload).ok()?.as_bytes());
    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = hmac_sha256(secret.as_bytes(), signing_input.as_bytes());
    Some(format!("{signing_input}.{}", base64url(&sig)))
}

// ---------------------------------------------------------------------------
// base64url（无 padding，JWT 标准字母表）
// ---------------------------------------------------------------------------

const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64url(data: &[u8]) -> String {
    let mut s = String::with_capacity((data.len() + 2) / 3 * 4);
    for c in data.chunks(3) {
        let n = ((c[0] as u32) << 16)
            | ((*c.get(1).unwrap_or(&0) as u32) << 8)
            | (*c.get(2).unwrap_or(&0) as u32);
        s.push(B64URL[((n >> 18) & 63) as usize] as char);
        s.push(B64URL[((n >> 12) & 63) as usize] as char);
        if c.len() > 1 {
            s.push(B64URL[((n >> 6) & 63) as usize] as char);
        }
        if c.len() > 2 {
            s.push(B64URL[(n & 63) as usize] as char);
        }
    }
    s
}

// ---------------------------------------------------------------------------
// SHA-256（FIPS 180-4）
// ---------------------------------------------------------------------------

#[rustfmt::skip]
const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 完整实现（单块/多块/任意长度，含标准 padding）。
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    // 消息 padding：0x80 + 0x00… + 64bit 大端位长，填充到 56 mod 64
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = Vec::with_capacity(data.len() + 72);
    msg.extend_from_slice(data);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    let mut w = [0u32; 64];
    for chunk in msg.chunks_exact(64) {
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut hh = h[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

// ---------------------------------------------------------------------------
// HMAC-SHA256（RFC 2104）
// ---------------------------------------------------------------------------

pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    // key > blocksize(64) 时先哈希；不足则右补零
    let mut k = [0u8; 64];
    if key.len() > 64 {
        let hk = sha256(key);
        k[..32].copy_from_slice(&hk);
    } else {
        k[..key.len()].copy_from_slice(key);
    }

    let mut inner = Vec::with_capacity(64 + msg.len());
    let mut outer = Vec::with_capacity(96);
    for i in 0..64 {
        inner.push(k[i] ^ 0x36);
        outer.push(k[i] ^ 0x5c);
    }
    inner.extend_from_slice(msg);
    outer.extend_from_slice(&sha256(&inner));
    sha256(&outer)
}

// ---------------------------------------------------------------------------
// 测试：标准向量 + JWT 结构（host target 跑：cargo test --lib）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn sha256_standard_vectors() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // 超过一个 block（>64B）的多块路径
        assert_eq!(
            hex(&sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn hmac_sha256_rfc4231_vectors() {
        // RFC 4231 Test Case 2（key 短于 block）
        assert_eq!(
            hex(&hmac_sha256(
                b"key",
                b"The quick brown fox jumps over the lazy dog"
            )),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
        // RFC 4231 Test Case 3（key 长于 block → 先哈希）
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn base64url_no_padding() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(b"foobar"), "Zm9vYmFy");
        // + / → - _ 的 JWT 字母表
        assert_eq!(base64url(&[0xfb, 0xff, 0xbf]), "-_-_");
    }

    #[test]
    fn zhipu_jwt_structure() {
        let key = "5bc8abc123.id-secret-xyz";
        let jwt = sign_zhipu_jwt(key, 1_700_000_000_000.0, 3_600_000.0).expect("jwt");
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT 必须是 header.payload.signature");

        // header
        let hdr = decode_b64url_json(parts[0]);
        assert_eq!(hdr["alg"], "HS256");
        assert_eq!(hdr["sign_type"], "SIGN");
        // payload
        let pl = decode_b64url_json(parts[1]);
        assert_eq!(pl["api_key"], "5bc8abc123");
        assert_eq!(pl["timestamp"], 1_700_000_000_000i64);
        assert_eq!(pl["exp"], 1_700_003_600_000i64);

        // signature 可用 secret 复算
        let sig = hmac_sha256(
            b"id-secret-xyz",
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
        );
        assert_eq!(parts[2], base64url(&sig));
    }

    #[test]
    fn zhipu_jwt_rejects_bad_key() {
        assert!(sign_zhipu_jwt("no-dot-here", 0.0, 1000.0).is_none());
        assert!(sign_zhipu_jwt(".onlysecret", 0.0, 1000.0).is_none());
        assert!(sign_zhipu_jwt("onlyid.", 0.0, 1000.0).is_none());
        // 硅基流动 key（sk- 开头无点）不走 JWT
        assert!(!looks_like_zhipu_key("sk-sf-fakekeyfakekeyfakekeyfakekey0000"));
        assert!(looks_like_zhipu_key("5bc8abc123.id-secret-xyz"));
    }

    /// 测试专用：JWT base64url（无 padding）解码，与 base64url() 互逆
    fn decode_b64url(s: &str) -> Vec<u8> {
        let mut bits: u32 = 0;
        let mut nbits = 0;
        let mut out = Vec::new();
        for c in s.bytes() {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'-' => 62,
                b'_' => 63,
                _ => panic!("bad b64url char"),
            } as u32;
            bits = (bits << 6) | v;
            nbits += 6;
            if nbits >= 8 {
                nbits -= 8;
                out.push((bits >> nbits) as u8);
            }
        }
        out
    }

    fn decode_b64url_json(s: &str) -> serde_json::Value {
        serde_json::from_slice(&decode_b64url(s)).expect("json")
    }
}
