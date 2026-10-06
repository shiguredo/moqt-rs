//! メディア時刻から壁時計への換算のテスト
//!
//! 公開 API (`WallClockMapper`) の契約を確認する。

use shiguredo_moqt::media_clock::WallClockMapper;

/// 記録が無く、代わりの壁時計も渡さないときは換算できない
#[test]
fn to_wall_clock_returns_none_without_observation() {
    let mut mapper = WallClockMapper::new();
    assert_eq!(
        mapper.to_wall_clock_us(1_000, None),
        None,
        "記録が無ければ換算できないこと"
    );
}

/// 代わりの壁時計を渡すと、そのフレームをその時刻に読んだものとして換算する
#[test]
fn to_wall_clock_uses_fallback_wall_clock() {
    let mut mapper = WallClockMapper::new();
    assert_eq!(
        mapper.to_wall_clock_us(1_000, Some(5_000)),
        Some(5_000),
        "代わりの壁時計を対応にすること"
    );
    assert_eq!(mapper.offset_us(), Some(4_000), "対応が記録されること");
}

/// 対応は遅れが最も小さいフレームで決まる
#[test]
fn observe_keeps_smallest_offset() {
    let mut mapper = WallClockMapper::new();
    mapper.observe(0, 1_000_000);
    mapper.observe(33_333, 1_001_000);
    assert_eq!(
        mapper.to_wall_clock_us(66_666, None),
        Some(66_666 + 967_667),
        "小さい方の対応を使うこと"
    );
}

/// 対応を小さくするときも、換算した時刻は前のフレームより戻らない
#[test]
fn conversion_moves_toward_smaller_offset_without_going_back() {
    let mut mapper = WallClockMapper::new();
    mapper.observe(0, 1_000_000);
    let first = mapper
        .to_wall_clock_us(0, None)
        .expect("記録したので換算できること");
    assert_eq!(first, 1_000_000, "最初は対応をそのまま使うこと");
    // 遅れの小さいフレームが届き、対応の目標が小さくなる
    mapper.observe(33_333, 933_333);
    let second = mapper
        .to_wall_clock_us(33_333, None)
        .expect("記録したので換算できること");
    assert!(
        second > first,
        "換算した時刻が前のフレームより戻らないこと first={first} second={second}"
    );
    assert!(
        second < 33_333 + 1_000_000,
        "対応が目標へ向けて動くこと second={second}"
    );
}

/// Unix epoch より前にはしない
#[test]
fn conversion_never_returns_negative() {
    let mut mapper = WallClockMapper::new();
    assert_eq!(
        mapper.to_wall_clock_us(-10_000, Some(-5_000)),
        Some(0),
        "負の値は 0 にすること"
    );
}

/// 記録を消すと換算できなくなる
#[test]
fn reset_clears_observation() {
    let mut mapper = WallClockMapper::new();
    mapper.observe(0, 1_000_000);
    mapper.reset();
    assert_eq!(
        mapper.to_wall_clock_us(0, None),
        None,
        "消したら換算できないこと"
    );
}
