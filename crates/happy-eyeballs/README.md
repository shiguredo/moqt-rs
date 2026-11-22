# happy-eyeballs

Happy Eyeballs v3 (draft-ietf-happy-happyeyeballs-v3-05) の接続候補スケジューラである。
Sans I/O の `#![no_std]` クレートであり、ソケット、DNS、タイマー、非同期タスクを扱わない。

呼び出し側は、トランスポートの優先順に並べた候補グループ、単調増加の時刻 (ミリ秒)、
接続試行の成功・失敗を渡す。スケジューラは次に開始する候補、次回開始時刻、勝者、
全候補失敗時の先頭の失敗を返す。

- `Config`: Connection Attempt Delay、最小値、最大値、Multiplier、
  Preferred Address Family Count
- `CandidateGroup<C>` / `Candidate<C>`: トランスポート種別と接続先アドレス
- `interleave_address_families`: アドレスファミリの交互配置と重複除去
- `scheduler::Scheduler<C>`: 接続試行の状態機械

`scheduler::Scheduler` は同期関数だけを提供する。Tokio などの I/O との接続は
`tokio-moq` のアダプタが行う。全体の設計と draft-05 との対応は
[`docs/happy-eyeballs.md`](../../docs/happy-eyeballs.md) を参照する。

```sh
cargo test -p happy-eyeballs
cargo clippy -p happy-eyeballs --all-targets -- -D warnings
```
