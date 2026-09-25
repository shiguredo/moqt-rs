//! エンコードの Property-Based Testing
//!
//! 任意の CBOR データ項目を生成し、通常エンコードと決定論的エンコードの
//! 往復で値が変わらないこと、エンコード結果が安定することを検証する。

use super::helpers;

use std::cell::Cell;

use shiguredo_moqt::cbor::decode::decode;
use shiguredo_moqt::cbor::value::Value;

/// このファイルの PBT ケース数
const CASES: usize = 512;

/// 生成した Value のクラスごとの到達状況
///
/// プロパティが空虚に成功していないことを確認するためのゲートに使う。
#[derive(Debug, Default)]
struct Coverage {
    /// コンテナやタグを要素に持つコンテナ
    nested: Cell<usize>,
    /// NaN・無限大・符号付きゼロなどの特殊な浮動小数点値
    special_float: Cell<usize>,
    /// タグ付きデータ項目
    tag: Cell<usize>,
    /// 32 以上の割り当てられていない simple value
    unassigned_simple: Cell<usize>,
    /// 空のバイト文字列・テキスト文字列・配列・マップ
    empty: Cell<usize>,
    /// エンコード長が変わる境界の整数
    boundary_integer: Cell<usize>,
    /// 256 バイト以上の文字列
    large_string: Cell<usize>,
}

impl Coverage {
    /// 生成された Value をクラスごとに数える
    fn observe(&self, value: &Value) {
        match value {
            Value::Array(items) => {
                if items.iter().any(is_container) {
                    self.nested.set(self.nested.get() + 1);
                }
                if items.is_empty() {
                    self.empty.set(self.empty.get() + 1);
                }
                for item in items {
                    self.observe(item);
                }
            }
            Value::Map(pairs) => {
                if pairs
                    .iter()
                    .any(|(key, value)| is_container(key) || is_container(value))
                {
                    self.nested.set(self.nested.get() + 1);
                }
                if pairs.is_empty() {
                    self.empty.set(self.empty.get() + 1);
                }
                for (key, value) in pairs {
                    self.observe(key);
                    self.observe(value);
                }
            }
            Value::Tag(_, content) => {
                self.tag.set(self.tag.get() + 1);
                self.observe(content);
            }
            Value::Float(value) if value.is_nan() || value.is_infinite() || *value == 0.0 => {
                self.special_float.set(self.special_float.get() + 1);
            }
            Value::Simple(value) if *value >= 32 => {
                self.unassigned_simple.set(self.unassigned_simple.get() + 1);
            }
            Value::Unsigned(value) | Value::Negative(value)
                if matches!(
                    *value,
                    0 | 23 | 24 | 255 | 256 | 65535 | 65536 | 4294967295 | 4294967296 | u64::MAX
                ) =>
            {
                self.boundary_integer.set(self.boundary_integer.get() + 1);
            }
            Value::ByteString(bytes) if bytes.len() >= 256 => {
                self.large_string.set(self.large_string.get() + 1);
            }
            Value::TextString(text) if text.len() >= 256 => {
                self.large_string.set(self.large_string.get() + 1);
            }
            _ => {}
        }
    }

    /// 全てのクラスが少なくとも 1 回は生成されたことを確認する
    ///
    /// ジェネレータの分布が偏るとプロパティが空虚に成功してしまうため、
    /// クラスごとに到達を数えて 0 件なら失敗させる。
    fn assert_reached(&self, runner: &noprop::Runner) {
        assert!(
            self.nested.get() > 0,
            "入れ子のコンテナが 1 件も生成されなかった\n{runner}"
        );
        assert!(
            self.special_float.get() > 0,
            "特殊な浮動小数点値が 1 件も生成されなかった\n{runner}"
        );
        assert!(
            self.tag.get() > 0,
            "タグ付きデータ項目が 1 件も生成されなかった\n{runner}"
        );
        assert!(
            self.unassigned_simple.get() > 0,
            "割り当てられていない simple value が 1 件も生成されなかった\n{runner}"
        );
        assert!(
            self.empty.get() > 0,
            "空のコンテナ・文字列が 1 件も生成されなかった\n{runner}"
        );
        assert!(
            self.boundary_integer.get() > 0,
            "境界の整数が 1 件も生成されなかった\n{runner}"
        );
        assert!(
            self.large_string.get() > 0,
            "256 バイト以上の文字列が 1 件も生成されなかった\n{runner}"
        );
    }
}

/// 配列・マップ・タグのいずれかかどうかを返す
fn is_container(value: &Value) -> bool {
    matches!(value, Value::Array(_) | Value::Map(_) | Value::Tag(..))
}

#[test]
fn encode_decode_round_trip() -> noprop::TestResult {
    let seed = noprop::seed_from_env_or_time("CBOR_RS_PBT_SEED")?;
    let coverage = Coverage::default();
    let mut runner = noprop::Runner::new(seed);

    runner.run(CASES, |ctx| {
        let value = helpers::sample_value(ctx, 0);
        coverage.observe(&value);

        // 通常エンコードの往復で値とバイト列が変わらないこと
        let bytes = value.to_bytes();
        let decoded = decode(&bytes).expect("生成した Value は必ずデコードできる");
        assert_eq!(decoded, value, "通常エンコードの往復で値が変わった");
        assert_eq!(decoded.to_bytes(), bytes, "通常エンコードが安定しない");

        // 決定論的エンコードが固定点になること
        let canonical = value.to_canonical_bytes();
        let decoded_canonical = decode(&canonical).expect("決定論的エンコードは必ずデコードできる");
        assert_eq!(
            decoded_canonical.to_canonical_bytes(),
            canonical,
            "決定論的エンコードが安定しない"
        );

        // 決定論的エンコードのマップのキーが辞書順になっていること
        assert_map_keys_sorted(&decoded_canonical);
        Ok(())
    })?;

    // ジェネレータがケース拒否をしていないこと
    assert_eq!(
        runner.stats().rejected_cases,
        0,
        "ケース拒否が発生した\n{runner}"
    );
    coverage.assert_reached(&runner);
    Ok(())
}

/// 決定論的エンコードされた値のマップのキーが辞書順になっていることを検証する
fn assert_map_keys_sorted(value: &Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                assert_map_keys_sorted(item);
            }
        }
        Value::Map(pairs) => {
            let mut previous: Option<Vec<u8>> = None;
            for (key, value) in pairs {
                let encoded = key.to_canonical_bytes();
                if let Some(previous) = &previous {
                    assert!(
                        previous <= &encoded,
                        "決定論的エンコードのマップのキーが辞書順になっていない: \
                         {previous:?} の後に {encoded:?}"
                    );
                }
                previous = Some(encoded);
                assert_map_keys_sorted(value);
            }
        }
        Value::Tag(_, content) => assert_map_keys_sorted(content),
        _ => {}
    }
}
