//! メディア時刻を Unix epoch の壁時計へ換算する処理
//!
//! LOC の TIMESTAMP は Timescale を載せない場合 Unix epoch からのマイクロ秒 (壁時計) と
//! して解釈される (draft-ietf-moq-loc-04 §2.3.1.1)。デバイスが返すメディア時刻は取得元
//! ごとに基準が異なるため、そのままでは使えない。
//!
//! フレームは撮ってから読むまでに遅れる。遅れは開始時に大きい。1 つのフレームだけで対応を
//! 取ると、その遅れの分だけ以降のすべての TIMESTAMP が撮った時刻より未来へずれる。そのため
//! 読んだフレームの差の最小値 (遅れが最も小さいフレーム) を対応にする。メディア時刻と壁時計
//! は同じ速さで進むため、対応は小さくする向きにだけ動かす。
//!
//! 時計もデバイスも触らず、フレームの時刻とそのとき読んだ壁時計を引数で受ける。

/// メディア時刻を壁時計へ換算する (映像のトラックごとに 1 つ持つ)
///
/// 時刻は呼び出し側が引数で渡す。
pub struct WallClockMapper {
    /// 読んだときの壁時計 - メディア時刻の最小値 (マイクロ秒)。換算に使う対応の目標
    target_offset_us: Option<i64>,
    /// 換算に使っている対応 (マイクロ秒)。目標へ向けて少しずつ動かす
    applied_offset_us: Option<i64>,
    /// 直前に換算したフレームのメディア時刻 (マイクロ秒)
    last_converted_media_us: Option<i64>,
}

impl WallClockMapper {
    /// まだ 1 つもフレームを記録していない状態で作る
    pub fn new() -> Self {
        Self {
            target_offset_us: None,
            applied_offset_us: None,
            last_converted_media_us: None,
        }
    }

    /// 読んだフレームを記録する (フレームを読むたびに呼ぶ)
    ///
    /// `media_us` はフレームのメディア時刻 (マイクロ秒)、`wall_clock_us` はフレームを
    /// 読んだときの壁時計 (Unix epoch マイクロ秒)。対応は小さくなる向きにだけ更新する。
    pub fn observe(&mut self, media_us: i64, wall_clock_us: i64) {
        let offset_us = wall_clock_us.saturating_sub(media_us);
        if self
            .target_offset_us
            .is_none_or(|target_offset_us| offset_us < target_offset_us)
        {
            self.target_offset_us = Some(offset_us);
        }
    }

    /// メディア時刻を壁時計 (Unix epoch マイクロ秒) へ換算する
    ///
    /// 対応を後から小さくすると、換算した TIMESTAMP が前のフレームより戻ることがある
    /// (受信側は TIMESTAMP の順と間隔を再生に使う)。換算に使う対応は目標へ向けて動かすが、
    /// 1 回の換算で動かす量を前に換算したフレームとのメディア時刻の差の半分未満に抑える。
    /// これにより換算した TIMESTAMP の差はメディア時刻の差の半分より大きく保たれ、単調に
    /// 増える。30 fps では 1 回あたり約 16.7 ms 未満であり、開始時の数百 ms の遅れは
    /// 1 秒ほどで埋まる。LOC の Timestamp は vi64 で負を表せないため、Unix epoch より前には
    /// しない。
    ///
    /// まだ 1 つも記録していないときは `fallback_wall_clock_us` をそのフレームを読んだ壁時計
    /// とみなして記録する。渡されていないときは `None` を返す。
    pub fn to_wall_clock_us(
        &mut self,
        media_us: i64,
        fallback_wall_clock_us: Option<i64>,
    ) -> Option<i64> {
        if self.target_offset_us.is_none() {
            self.observe(media_us, fallback_wall_clock_us?);
        }
        let target_offset_us = self.target_offset_us?;
        let mut applied_offset_us = self.applied_offset_us.unwrap_or(target_offset_us);
        if target_offset_us < applied_offset_us
            && let Some(last_converted_media_us) = self.last_converted_media_us
        {
            // メディア時刻の差の半分未満 (1 マイクロ秒引く) だけ動かす
            let step_us = media_us
                .saturating_sub(last_converted_media_us)
                .saturating_div(2)
                .saturating_sub(1)
                .max(0);
            applied_offset_us = applied_offset_us
                .saturating_sub(step_us)
                .max(target_offset_us);
        }
        self.applied_offset_us = Some(applied_offset_us);
        self.last_converted_media_us = Some(media_us);
        Some(media_us.saturating_add(applied_offset_us).max(0))
    }

    /// 換算に使っている対応 (マイクロ秒)。まだ換算していなければ `None`
    pub fn offset_us(&self) -> Option<i64> {
        self.applied_offset_us
    }

    /// 記録した対応と換算の状態を消す
    pub fn reset(&mut self) {
        self.target_offset_us = None;
        self.applied_offset_us = None;
        self.last_converted_media_us = None;
    }
}

impl Default for WallClockMapper {
    fn default() -> Self {
        Self::new()
    }
}
