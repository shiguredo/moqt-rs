//! CBOR コーデックのテスト
//!
//! 本ファイルには integration test 共有の helper を置き、
//! 実際の公開 API 検証は `test_cbor/` 配下の責務別サブモジュールへ分割する。

/// 16 進数の文字列をバイト列に変換する
fn hex_to_bytes(hex: &str) -> Vec<u8> {
    assert!(hex.len().is_multiple_of(2), "16 進数の桁数が奇数: {hex}");
    (0..hex.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&hex[index..index + 2], 16)
                .expect("16 進数の変換に失敗するはずがない (テストデータの誤り)")
        })
        .collect()
}

/// バイト列を 16 進数の文字列に変換する
fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[path = "test_cbor/appendix_a.rs"]
mod appendix_a;
#[path = "test_cbor/decode.rs"]
mod decode;
#[path = "test_cbor/diagnostic.rs"]
mod diagnostic;
#[path = "test_cbor/encode.rs"]
mod encode;
#[path = "test_cbor/value.rs"]
mod value;
