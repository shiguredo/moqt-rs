# MSF Timeline の時刻フィールドの数値型と検証を見直す

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/change-msf-timeline-number-type
- Polished: {YYYY-MM-DD}

## 目的

MSF-01 §7.1.1 (Media Timeline) と §8.1 (Event Timeline)、§7.4.1 (Template) は時刻を JSON Number と規定し、非負であることを明示していない。`src/msf.rs` は `u64` と `parse_u64` で実装しているため、負値 (`-1`) と小数表記 (`2002.0`) を拒否する。仕様の解釈と実装の制限を一致させ、制限を残すなら公開 doc とテストで固定する。

## 現状

- `parse_u64` は負値と小数を拒否する。
- Media Timeline の `pts_ms`、Event Timeline の `MediaPtsMs`、Template の `startMediaTime` / `deltaMediaTime` は u64 である。
- 仕様は JSON Number としか規定しておらず、整数か小数か、負を許すかを明示しない。
- 影響: 他実装が小数で送ると decode に失敗する。負の PTS を送る実装と相互運用できない。

## 設計方針

- i64 と「整数 ms であること」の検証に変える案と、非負整数に限定する制限を doc とテストで固定する案を比較する。
- 小数を受け入れる場合は丸め規則を決める。
- 決めた契約を `docs/msf.md` に明記する。

## 完了条件

- 時刻フィールドの受理範囲が仕様解釈と一致し、doc に明記されていること。
- 境界値のテストが追加されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
