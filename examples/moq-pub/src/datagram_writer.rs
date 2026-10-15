//! Object Datagram ライター
//!
//! 1 つの Group に対応する Object Datagram を送信する。
//! draft-ietf-moq-transport-22 §11.2.1 (Object Datagram) に準拠する。

use shiguredo_moqt::loc::LocProperties;
use shiguredo_moqt::message::common::Location;
use shiguredo_moqt::stream::datagram::ObjectDatagram;

use crate::error::{Error, Result};
use tokio_moq::moqt_client::{DataPlaneHandle, ObjectFilterOutcome};
use tokio_moq::transport;

/// Object Datagram ライター
///
/// draft-ietf-moq-transport-22 §11.2.1 (Object Datagram) の OBJECT_DATAGRAM を送信する。
/// 上限サイズを超える object は送らずにエラーにする (datagram は 1 つの QUIC パケットに
/// 収まる必要がある)。
pub struct DatagramWriter {
    handle: transport::StreamHandle,
    data_plane: DataPlaneHandle,
    request_id: u64,
    track_alias: u64,
    group_id: u64,
    publisher_priority: u8,
    /// object datagram の上限サイズ (bytes)
    ///
    /// datagram は 1 つの QUIC パケットに収まる必要がある (draft-ietf-moq-loc-04 §4.1)。
    /// 上限を超える object は送らずにエラーにする。
    max_size: usize,
    /// 同じ Object の datagram をワイヤへ出す回数
    ///
    /// datagram は再送されない (RFC 9221 §3) ため、n 回送るとすべての copy が落ちる確率が
    /// 損失率の n 乗になる。1〜3 の範囲は送信時に [`datagram_copies`] で制限する。
    redundancy: u8,
    /// 次に書き込むオブジェクト ID
    next_object_id: u64,
}

impl DatagramWriter {
    /// 新しい DatagramWriter を作成する
    ///
    /// stream は開かない。datagram 送信ごとにヘッダを生成して送信する。
    /// `redundancy` は同じ Object を送る回数であり、1〜3 の外は送信時に
    /// [`datagram_copies`] で制限する (CLI は 1〜3 の整数だけを受理する)。
    #[expect(
        clippy::too_many_arguments,
        reason = "Group の Object を datagram で送るために必要な情報を 1 回で渡すため"
    )]
    pub fn new(
        handle: &transport::StreamHandle,
        data_plane: &DataPlaneHandle,
        request_id: u64,
        track_alias: u64,
        group_id: u64,
        publisher_priority: u8,
        max_size: usize,
        redundancy: u8,
    ) -> Self {
        Self {
            handle: handle.clone(),
            data_plane: data_plane.clone(),
            request_id,
            track_alias,
            group_id,
            publisher_priority,
            max_size,
            redundancy,
            next_object_id: 0,
        }
    }

    /// 次に [`DatagramWriter::write_object`] が送る Object の Location を返す
    ///
    /// FETCH 応答のために保持する Location と、OBJECT_DATAGRAM に載る Object ID を
    /// 一致させるために使う。
    pub fn next_object_location(&self) -> Location {
        Location {
            group_id: self.group_id,
            object_id: self.next_object_id,
        }
    }

    /// オブジェクトを datagram で送信する
    ///
    /// LOC プロパティとペイロードを含むオブジェクトを datagram で送信する。
    /// Session のフィルタ評価を先に行い、通過時のみワイヤへ出す
    /// (draft-ietf-moq-transport-22 §3.3.3 (Combining Filters): "The publisher MUST forward
    /// only objects that pass all filters.")。フィルタ不通過
    /// (`SendRequestError::LocalFilterMismatch`) のオブジェクトはワイヤに出さず `Skip` を返す。
    /// オブジェクト ID だけ進める (datagram は絶対 ID のため delta の考慮は不要)。
    ///
    /// 冗長度が 2 以上なら同じ Object の datagram をその回数だけワイヤへ出す。datagram は
    /// 再送されない (RFC 9221 §3) ため、すべての copy が落ちる確率が損失率の冗長度乗になる。
    /// フィルタ評価とサイズ判定は 1 回だけ行い、Object ID も 1 回だけ進めるため、copy は
    /// すべて同じ Object として届く (受信側は同じ識別子の 2 回目を捨てる)。
    ///
    /// copy は間隔を空けずに連続して送る。2 個目以降を間隔を空けて送る案は、moqt-js の
    /// devtools (`devtools/src/hooks/usePublisher.ts`) が実測で棄却している。遅れて届く復旧の
    /// 分が受信側の遅れの推定を膨らませ、交互に各 3 回の比較で鳴り始めの遅れが 64〜67 ms から
    /// 120〜122 ms へ上がった (位置の順序で除く案でも改善しなかった)。
    ///
    /// 返り値の `ObjectFilterOutcome::Pass` はワイヤへ送信したこと、`Skip` はフィルタ
    /// 不通過で送信しなかったことを示す。呼び出し側は `Skip` をスキップとして扱い、
    /// 送信を継続する (その他のエラーは伝播する)。
    ///
    /// 検証: 送信回数の決定は [`datagram_copies`] の単体テストで固定する。トランスポートへ
    /// 実際に何回出たかは実トランスポート (`transport::StreamHandle`) と実リレーが要るため
    /// 単体テストでは固定しない (`e2e-tests/` の担当である)。
    ///
    /// 検証 (手動): OBJECT_PROPERTY_FILTER 付きの subscription に対して、
    /// フィルタ外のオブジェクトが subscriber に届かず publisher が生存すること、
    /// フィルタを通過するオブジェクトの配信が従来どおり行われることを確認する。
    /// リポジトリ内の moq-sub はフィルタ付き SUBSCRIBE を発行する手段を
    /// 持たないため、 subscriber 側にフィルタを付与する改変が必要である。
    pub async fn write_object(
        &mut self,
        payload: &[u8],
        properties: &LocProperties,
    ) -> Result<ObjectFilterOutcome> {
        let object_id = self.next_object_id;

        // LOC プロパティをエンコードする
        let properties_data = if properties.is_empty() {
            None
        } else {
            Some(properties.encode()?)
        };

        let datagram = ObjectDatagram {
            track_alias: self.track_alias,
            group_id: self.group_id,
            object_id,
            publisher_priority: Some(self.publisher_priority),
            properties_data,
            end_of_group: false,
            status: None,
        };

        let header = datagram.encode()?;
        let mut buf = Vec::with_capacity(header.len() + payload.len());
        buf.extend_from_slice(&header);
        buf.extend_from_slice(payload);

        // フィルタ評価は doc のとおりワイヤ送信より先に行う
        let outcome = self.data_plane.send_object_datagram(
            self.request_id,
            self.group_id,
            object_id,
            datagram.properties_data,
            None,
            datagram.end_of_group,
        )?;
        if outcome == ObjectFilterOutcome::Skip {
            tracing::debug!(
                "skip object datagram (filter mismatch): request_id={}, object_id={}",
                self.request_id,
                object_id,
            );
            self.next_object_id = object_id + 1;
            return Ok(ObjectFilterOutcome::Skip);
        }

        // RFC 9221 §3 は peer が広告した max_datagram_frame_size を超える DATAGRAM の送信を
        // MUST NOT とし、受信側は PROTOCOL_VIOLATION で接続を閉じる MUST を負う。path MTU を
        // 超えた datagram は s2n-quic が通知なく破棄し、draft-ietf-moq-transport-22 §11.2 も
        // 「上限を超えた object は通知なく破棄される」と定める。送信前にサイズを判定して
        // 原因と対処が分かるエラーにする。
        check_datagram_size(buf.len(), self.max_size)?;

        // 同じ Object の datagram を決めた回数だけ出す。フィルタ評価・サイズ判定・Object ID の
        // 前進はここまでの 1 回だけで済んでおり、copy はすべて同じ Object になる
        let copies = datagram_copies(self.redundancy);
        if copies > 1 {
            tracing::debug!(
                "sending redundant object datagrams: request_id={}, object_id={}, copies={}",
                self.request_id,
                object_id,
                copies,
            );
        }
        for _ in 0..copies {
            self.handle.send_datagram(&buf).await?;
        }

        self.next_object_id = object_id + 1;

        Ok(ObjectFilterOutcome::Pass)
    }
}

/// object datagram のサイズが上限内かどうかを判定する
///
/// datagram は分割できず (RFC 9221 §5 "DATAGRAM frames cannot be fragmented")、1 つの QUIC
/// パケットに収まる必要があるため、送信前にこの判定を行う。
fn check_datagram_size(size: usize, max_size: usize) -> Result<()> {
    if size > max_size {
        return Err(Error::DatagramTooLarge { size, max_size });
    }
    Ok(())
}

/// 同じ Object を datagram で送る回数を決める
///
/// 回数は 1〜3 に制限する。0 は datagram が 1 つも送られず、音が届かないことに気づけない
/// まま `ObjectFilterOutcome::Pass` を返してしまう。4 以上は音声の帯域と datagram の数を
/// 無駄にする。CLI も 1〜3 の整数だけを受理するが、`Config` は公開されており、呼び出し側が
/// 直接組み立てて `pipeline::run` へ渡せるため、送信側でも同じ範囲に制限する (二重の防御)。
fn datagram_copies(redundancy: u8) -> u8 {
    redundancy.clamp(1, 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 上限と等しいサイズは送信でき、上限を超えるとエラーになる
    ///
    /// datagram は 1 つの QUIC パケットに収まる必要があるため (draft-ietf-moq-loc-04 §4.1)、
    /// 境界 (上限未満 / 上限と等しい / 上限 + 1) を固定する。
    #[test]
    fn datagram_size_boundary() {
        let max_size = crate::cli::DEFAULT_DATAGRAM_MAX_SIZE;
        assert!(
            check_datagram_size(max_size - 1, max_size).is_ok(),
            "上限未満のサイズは送信できること"
        );
        assert!(
            check_datagram_size(max_size, max_size).is_ok(),
            "上限と等しいサイズは送信できること"
        );

        let error = check_datagram_size(max_size + 1, max_size)
            .expect_err("上限を超えるとエラーになること");
        assert!(
            matches!(
                error,
                Error::DatagramTooLarge { size, max_size: limit }
                    if size == crate::cli::DEFAULT_DATAGRAM_MAX_SIZE + 1
                        && limit == crate::cli::DEFAULT_DATAGRAM_MAX_SIZE
            ),
            "実サイズと上限が保持されること: {error}"
        );
    }

    /// 送信回数は 1〜3 に制限される
    ///
    /// CLI は 1〜3 の整数だけを受理するが、`Config` を直接組み立てて `pipeline::run` へ渡す
    /// 利用では範囲外の値が入りうる。0 のままだと datagram が 1 つも送られずに `Pass` を
    /// 返し、音が無いことに気づけないため、送信側でも同じ範囲に制限する。
    ///
    /// トランスポートへ実際に何回出たかは実トランスポートと実リレーが要るため固定しない。
    /// ここでは回数の決定だけを固定する。
    #[test]
    fn datagram_copies_clamps_redundancy() {
        let cases: &[(u8, u8)] = &[(0, 1), (1, 1), (2, 2), (3, 3), (4, 3), (u8::MAX, 3)];
        for &(redundancy, expected) in cases {
            assert_eq!(
                datagram_copies(redundancy),
                expected,
                "送信回数が 1〜3 に制限されること: redundancy={redundancy}"
            );
        }
    }
}
