#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::stream::OBJECT_STATUS_END_OF_GROUP;
use shiguredo_moqt::stream::OBJECT_STATUS_END_OF_TRACK;
use shiguredo_moqt::stream::subgroup::SubgroupObject;

/// `SubgroupObject::encode` に与える入力
///
/// `has_properties` と `properties_data` を任意に生成し、Properties の検証
/// (Properties Length varint が途中で切れている・宣言長と実データ長が一致しない) の
/// 拒否経路へ到達させる。panic しないことだけを検証する。
#[derive(Arbitrary, Debug)]
struct Input {
    /// Object ID Delta (検証対象ではないが、encode が書き出す値として任意に振る)
    object_id_delta: u64,
    /// payload_length と status の組み合わせの選択
    shape: u8,
    /// SUBGROUP_HEADER の PROPERTIES ビットに対応する
    has_properties: bool,
    /// Properties Length varint + Properties データの生バイト列
    properties_data: Vec<u8>,
}

fuzz_target!(|input: Input| {
    // payload_length と status の組み合わせは encode が要求する契約
    // (payload_length == 0 なら status 必須、payload_length > 0 なら status 禁止) を
    // 満たす 3 通りに固定する。ここを任意値にすると契約違反で先に弾かれ、
    // 目的の Properties 検証経路に到達しなくなる
    let (payload_length, status) = match input.shape % 3 {
        // status なしで payload_length > 0
        0 => (1, None),
        // payload_length == 0 で End of Group (Properties Length > 0 の拒否経路も通る)
        1 => (0, Some(OBJECT_STATUS_END_OF_GROUP)),
        // payload_length == 0 で End of Track (Properties Length > 0 の拒否経路も通る)
        _ => (0, Some(OBJECT_STATUS_END_OF_TRACK)),
    };
    let object = SubgroupObject {
        object_id_delta: input.object_id_delta,
        payload_length,
        status,
    };
    // has_properties が false のときに properties_data を渡すのは契約違反のため、
    // フラグに応じて None / Some を切り替える
    let properties_data = input
        .has_properties
        .then_some(input.properties_data.as_slice());
    let mut buf = Vec::new();
    let _ = object.encode(input.has_properties, properties_data, &mut buf);
});
