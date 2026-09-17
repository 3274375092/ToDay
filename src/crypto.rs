//! 密钥派生与字段加密。
//!
//! 主密码 → Argon2id → 32 字节密钥 → ChaCha20-Poly1305 逐字段加解密。
//! 密钥只活在 [`Session`] 里，不落盘、不上抛；上层能拿到的只有密文和明文。
//!
//! 每条密文都用日期当 AAD（附加认证数据），所以把某天的密文搬到另一天解不开。

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore;
use zeroize::Zeroizing;

/// 密钥长度：ChaCha20-Poly1305 要 256 位
pub const KEY_LEN: usize = 32;
/// 盐长度
pub const SALT_LEN: usize = 16;
/// nonce 长度：密文的前 12 字节
const NONCE_LEN: usize = 12;
/// 密文格式版本。换算法或换 AAD 规则时 +1，`entries.format` 靠它区分新老行。
pub const FORMAT_VERSION: u32 = 1;

/// 验证块里的固定明文与 AAD：能用派生密钥解开，就说明密码对
const VERIFIER_PLAINTEXT: &str = "ToDay verifier v1";
const VERIFIER_AAD: &str = "kdf.verifier";

/// 派生密钥。类型本身会在 drop 时清零，不需要调用方额外擦除。
pub type Key = Zeroizing<[u8; KEY_LEN]>;

/// 加解密相关的失败
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("KDF 参数不合法：{0}")]
    Params(String),
    #[error("密钥派生失败：{0}")]
    Kdf(String),
    #[error("加密失败")]
    Seal,
    #[error("解密失败（密码不对或数据损坏）")]
    Open,
    #[error("KDF 参数串无法解析：{0}")]
    MalformedParams(String),
}

/// Argon2id 的代价参数与盐。
///
/// 参数写进库里的 `meta` 表，将来调参不用改代码就能读老库：
/// 每个库带着自己那一套参数，新库用新参数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KdfParams {
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub salt: [u8; SALT_LEN],
}

impl KdfParams {
    /// 随机盐 + OWASP 的最低推荐档（m=19 MiB, t=2, p=1）
    pub fn generate() -> Self {
        let mut salt = [0u8; SALT_LEN];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        Self {
            m_cost: Params::DEFAULT_M_COST,
            t_cost: Params::DEFAULT_T_COST,
            p_cost: Params::DEFAULT_P_COST,
            salt,
        }
    }

    /// 参数串：`argon2id$m=19456,t=2,p=1$<hex salt>`
    ///
    /// 盐放在里面而不是单独一列：参数只有一个来源，不会出现"盐和参数对不上"。
    pub fn encode(&self) -> String {
        format!(
            "argon2id$m={},t={},p={}${}",
            self.m_cost,
            self.t_cost,
            self.p_cost,
            to_hex(&self.salt)
        )
    }

    /// 解析 [`KdfParams::encode`] 写出的参数串
    pub fn decode(text: &str) -> Result<Self, CryptoError> {
        let malformed = || CryptoError::MalformedParams(text.to_owned());

        let mut parts = text.split('$');
        let (algorithm, costs, salt) = (
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default(),
        );
        if algorithm != "argon2id" || parts.next().is_some() {
            return Err(malformed());
        }

        let (mut m_cost, mut t_cost, mut p_cost) = (None, None, None);
        for part in costs.split(',') {
            let (key, value) = part.split_once('=').ok_or_else(malformed)?;
            let value: u32 = value.parse().map_err(|_| malformed())?;
            match key {
                "m" => m_cost = Some(value),
                "t" => t_cost = Some(value),
                "p" => p_cost = Some(value),
                _ => return Err(malformed()),
            }
        }

        let salt: [u8; SALT_LEN] = from_hex(salt)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(malformed)?;

        Ok(Self {
            m_cost: m_cost.ok_or_else(malformed)?,
            t_cost: t_cost.ok_or_else(malformed)?,
            p_cost: p_cost.ok_or_else(malformed)?,
            salt,
        })
    }

    /// 组装好的 Argon2id 实例
    fn argon2(&self) -> Result<Argon2<'static>, CryptoError> {
        let params = Params::new(self.m_cost, self.t_cost, self.p_cost, Some(KEY_LEN))
            .map_err(|e| CryptoError::Params(e.to_string()))?;
        Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
    }
}

/// 从主密码派生密钥。
///
/// 派生期间密码会另存一份可清零的拷贝，函数返回时擦掉；
/// 调用方那边的原始密码由调用方自己用 [`Zeroizing`] 管好。
pub fn derive_key(password: &str, params: &KdfParams) -> Result<Key, CryptoError> {
    let secret = Zeroizing::new(password.as_bytes().to_vec());
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    params
        .argon2()?
        .hash_password_into(&secret, &params.salt, &mut key[..])
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    Ok(key)
}

/// 加密一段正文。
///
/// `aad` 是附加认证数据：这里固定传日期字符串，密文跟着日期一起被认证，
/// 挪到别的日期就解不开。返回 `nonce(12B) || 密文(自带 16B tag)`。
pub fn seal(key: &Key, aad: &str, plaintext: &str) -> Result<Vec<u8>, CryptoError> {
    let mut nonce = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce);

    let sealed = cipher(key)
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext.as_bytes(),
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| CryptoError::Seal)?;

    let mut blob = Vec::with_capacity(NONCE_LEN + sealed.len());
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&sealed);
    Ok(blob)
}

/// 解开一段正文，`aad` 与日期必须和加密时一致
pub fn open(key: &Key, aad: &str, blob: &[u8]) -> Result<String, CryptoError> {
    let (nonce, ciphertext) = blob.split_at_checked(NONCE_LEN).ok_or(CryptoError::Open)?;
    let plain = cipher(key)
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| CryptoError::Open)?;
    String::from_utf8(plain).map_err(|_| CryptoError::Open)
}

/// 写入 `meta.kdf.verifier` 的验证块。
///
/// 有了它，解锁时不碰 `entries` 也能判断密码对不对，空库同样有效。
pub fn verifier(key: &Key) -> Result<Vec<u8>, CryptoError> {
    seal(key, VERIFIER_AAD, VERIFIER_PLAINTEXT)
}

/// 密码对不对：能解开验证块就是对
pub fn verify(key: &Key, blob: &[u8]) -> bool {
    matches!(open(key, VERIFIER_AAD, blob), Ok(text) if text == VERIFIER_PLAINTEXT)
}

/// 解锁后活着的会话：拿着密钥，锁上就没了。
pub struct Session {
    key: Key,
}

impl Session {
    pub fn new(key: Key) -> Self {
        Self { key }
    }

    /// 加密一个字段，`aad` 传日期字符串
    pub fn seal(&self, aad: &str, plaintext: &str) -> Result<Vec<u8>, CryptoError> {
        seal(&self.key, aad, plaintext)
    }

    /// 解开一个字段
    pub fn open(&self, aad: &str, blob: &[u8]) -> Result<String, CryptoError> {
        open(&self.key, aad, blob)
    }

    /// 验证块对得上吗
    pub fn matches_verifier(&self, blob: &[u8]) -> bool {
        verify(&self.key, blob)
    }
}

/// 同一个密钥的密码器（构造很便宜，不用缓存）
fn cipher(key: &Key) -> ChaCha20Poly1305 {
    let bytes: &[u8; KEY_LEN] = key;
    ChaCha20Poly1305::new(bytes.into())
}

/// 小写十六进制
fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

/// 解析十六进制，遇到非十六进制字符或奇数长度返回 None
fn from_hex(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        out.push((high * 16 + low) as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let bytes: Vec<u8> = (0..=255u8).collect();
        assert_eq!(from_hex(&to_hex(&bytes)), Some(bytes));
    }

    #[test]
    fn hex_rejects_broken_input() {
        assert_eq!(from_hex("abc"), None, "奇数长度");
        assert_eq!(from_hex("zz"), None, "非十六进制字符");
    }
}
