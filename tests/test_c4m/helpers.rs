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
