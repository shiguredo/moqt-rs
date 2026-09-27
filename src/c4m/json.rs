//! nojson の共通処理

use alloc::string::String;
use hashbrown::HashSet;
use nojson::RawJsonValue;

/// オブジェクトのメンバー名が重複している場合にその名前を返す
///
/// RFC 7515 §4 と RFC 7517 §4 はメンバー名の一意性を MUST とし、重複した場合は
/// 拒否するか「字句的に最後」の重複だけを使うことを求める。nojson は最初の一致を
/// 返すため、重複を検出して呼び出し側が拒否できるようにする。オブジェクト以外の
/// 値では `None` を返す。
pub(crate) fn find_duplicate_member(
    value: RawJsonValue<'_, '_>,
) -> Result<Option<String>, nojson::JsonParseError> {
    let Ok(members) = value.to_object() else {
        return Ok(None);
    };
    let mut seen: HashSet<String> = HashSet::new();
    for (key, _) in members {
        let name = key.to_unquoted_string_str()?.into_owned();
        if !seen.insert(name.clone()) {
            return Ok(Some(name));
        }
    }
    Ok(None)
}
