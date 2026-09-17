//! 加密层的验收：密钥派生确定、密文可往返、篡改会被发现。

use ToDay::crypto::{self, KdfParams, Session};

fn params() -> KdfParams {
    KdfParams {
        m_cost: 19 * 1024,
        t_cost: 2,
        p_cost: 1,
        salt: [7u8; crypto::SALT_LEN],
    }
}

#[test]
fn same_password_and_salt_derive_the_same_key() {
    let first = crypto::derive_key("正确的马电池订书钉", &params()).unwrap();
    let second = crypto::derive_key("正确的马电池订书钉", &params()).unwrap();
    assert_eq!(&first[..], &second[..]);
}

#[test]
fn different_salt_derives_a_different_key() {
    let mut other = params();
    other.salt = [8u8; crypto::SALT_LEN];
    let first = crypto::derive_key("同一个密码", &params()).unwrap();
    let second = crypto::derive_key("同一个密码", &other).unwrap();
    assert_ne!(&first[..], &second[..]);
}

#[test]
fn different_password_derives_a_different_key() {
    let first = crypto::derive_key("密码甲", &params()).unwrap();
    let second = crypto::derive_key("密码乙", &params()).unwrap();
    assert_ne!(&first[..], &second[..]);
}

#[test]
fn seal_and_open_round_trip() {
    let key = crypto::derive_key("密码", &params()).unwrap();
    let aad = "2025-09-17";
    let plaintext = "今天把加密接上了，正文是中文、emoji 🎈 和换行\n都该原样回来";

    let blob = crypto::seal(&key, aad, plaintext).unwrap();
    assert_eq!(crypto::open(&key, aad, &blob).unwrap(), plaintext);
}

#[test]
fn ciphertext_never_contains_the_plaintext() {
    let key = crypto::derive_key("密码", &params()).unwrap();
    let marker = "机密暗号QUASAR1234";
    let blob = crypto::seal(&key, "2025-09-17", marker).unwrap();

    assert!(
        !blob
            .windows(marker.len())
            .any(|window| window == marker.as_bytes()),
        "密文里不该出现明文片段"
    );
}

#[test]
fn the_same_plaintext_encrypts_differently_each_time() {
    let key = crypto::derive_key("密码", &params()).unwrap();
    let first = crypto::seal(&key, "2025-09-17", "同一句话").unwrap();
    let second = crypto::seal(&key, "2025-09-17", "同一句话").unwrap();
    assert_ne!(first, second, "nonce 每次都要新随机，否则两份密文能对上");
}

#[test]
fn open_rejects_a_different_aad() {
    let key = crypto::derive_key("密码", &params()).unwrap();
    let blob = crypto::seal(&key, "2025-09-17", "那天的内容").unwrap();

    assert!(
        crypto::open(&key, "2025-09-18", &blob).is_err(),
        "把密文挪到别的日期必须解不开"
    );
}

#[test]
fn open_rejects_a_flipped_bit() {
    let key = crypto::derive_key("密码", &params()).unwrap();
    let blob = crypto::seal(&key, "2025-09-17", "那天的内容").unwrap();

    for index in [0, crypto::SALT_LEN.min(blob.len() - 1), blob.len() - 1] {
        let mut broken = blob.clone();
        broken[index] ^= 0x01;
        assert!(
            crypto::open(&key, "2025-09-17", &broken).is_err(),
            "第 {index} 字节被改动，必须解不开"
        );
    }
}

#[test]
fn open_rejects_a_truncated_blob() {
    let key = crypto::derive_key("密码", &params()).unwrap();
    let blob = crypto::seal(&key, "2025-09-17", "那天的内容").unwrap();

    assert!(crypto::open(&key, "2025-09-17", &blob[..4]).is_err());
    assert!(crypto::open(&key, "2025-09-17", &[]).is_err());
}

#[test]
fn verifier_accepts_the_right_password_and_rejects_others() {
    let key = crypto::derive_key("对的密码", &params()).unwrap();
    let blob = crypto::verifier(&key).unwrap();

    let right = Session::new(crypto::derive_key("对的密码", &params()).unwrap());
    assert!(right.matches_verifier(&blob));

    let wrong = Session::new(crypto::derive_key("错的密码", &params()).unwrap());
    assert!(!wrong.matches_verifier(&blob));
}

#[test]
fn kdf_params_survive_a_round_trip() {
    let original = KdfParams::generate();
    let text = original.encode();
    assert!(text.starts_with("argon2id$"));

    let decoded = KdfParams::decode(&text).unwrap();
    assert_eq!(decoded, original, "参数串要能原样读回来");
    assert_ne!(original.salt, [0u8; crypto::SALT_LEN], "盐必须随机");
}

#[test]
fn generated_params_use_the_recommended_costs() {
    let params = KdfParams::generate();
    assert_eq!(params.m_cost, 19 * 1024, "至少 19 MiB 内存");
    assert_eq!(params.t_cost, 2);
    assert_eq!(params.p_cost, 1);
    assert_ne!(params.salt, KdfParams::generate().salt, "每次都要新盐");
}

#[test]
fn decode_rejects_broken_param_strings() {
    for broken in [
        "",
        "argon2id",
        "argon2i$m=19456,t=2,p=1$00112233445566778899aabbccddeeff",
        "argon2id$m=19456,t=2,p=1",
        "argon2id$m=19456,t=2$00112233445566778899aabbccddeeff",
        "argon2id$m=19456,t=2,p=1$短",
        "argon2id$m=19456,t=2,p=1$00112233445566778899aabbccddeeff$多出来的一段",
        "argon2id$m=很多,t=2,p=1$00112233445566778899aabbccddeeff",
    ] {
        assert!(
            KdfParams::decode(broken).is_err(),
            "这个参数串应该被拒绝：{broken}"
        );
    }
}
