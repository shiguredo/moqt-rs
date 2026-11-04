//! テスト用のヘルパー

/// hex 文字列をバイト列に変換する
///
/// テストベクタの hex は小文字だけなので、小文字 hex に限定する。
pub fn decode_hex(text: &str) -> Vec<u8> {
    assert!(
        text.len().is_multiple_of(2),
        "hex 文字列の長さが偶数ではない: {text}"
    );
    (0..text.len() / 2)
        .map(|index| {
            u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
                .expect("hex 文字列として不正なバイトがある")
        })
        .collect()
}

/// バイト列を小文字 hex 文字列に変換する
pub fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::new();
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// テスト専用に生成した固定の ES384 公開鍵の x 座標 (hex)
#[cfg(feature = "aws-lc-rs")]
pub const ES384_PUBLIC_KEY_X_HEX: &str = "ce7de2ef769603fc91f4682efeedc9e415b221a79067153a31d8f2f62d14044e18be87058146596041b2148233ed4073";

/// テスト専用に生成した固定の ES384 公開鍵の y 座標 (hex)
#[cfg(feature = "aws-lc-rs")]
pub const ES384_PUBLIC_KEY_Y_HEX: &str = "8a69f5b6b5bdb0200d39bf760420a3da5bf091b8e81557f5cb4d37f7ce36f7a07fb1119a736e2385d75e4c3f5ca604e7";

/// テスト専用に生成した固定の ES384 秘密鍵 (hex)
#[cfg(feature = "aws-lc-rs")]
pub const ES384_PRIVATE_KEY_HEX: &str = "e3129db6c869a183e6ed6abc233723f7339b94e38c38bdde7c19d4674c5c2730eca7680ac6afa8d660810a8adead31e2";
