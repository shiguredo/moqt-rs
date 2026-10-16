# 未使用の expand と AudioDelayFeedback::reset を削除する

- Created: 2026-10-17
- Completed: 2026-10-17
- Branch: feature/remove-unused-playout-apis

## 目的

moqt-js は 2026-10-17 の変更 (`origin/develop` の 5fed7505「playout のレビュー指摘を反映
する」) で、playout の使われていない API を削除した。時間伸長 (`expandSamples`) と、
`AudioDelayFeedback` の `reset()` である。

moqt-rs の `src/playout/stretch.rs` の `expand` と、`src/playout/feedback.rs` の
`AudioDelayFeedback::reset` は、同じく production (`src/` と `examples/`) から呼ばれて
おらず、テストだけが使っている。公開 API の削除 (破壊的変更) だが、CODEBASE.md の
「破壊的変更は積極的に行う」「設計変更も積極的に行う」に従い、使われていない API を
残さない。

## 現状

- `src/playout/stretch.rs` の `expand` (時間伸長) は、波形の周期 1 つ分を挿して音を伸ばす。
  `compress` と対になる API として実装されているが、`src/` と `examples/` に呼び出しが
  無く、`tests/test_playout/stretch.rs` と `pbt/tests/prop_playout/stretch.rs` だけが使って
  いる。
- この再生は音を伸ばす場面を持たない。目標より早く届いた音は前の音の終わりより後ろに置けば
  よく、足りない区間は補間 (`conceal`) が埋める。moqt-js も同じ理由で時間伸長を削除して
  いる (`src/playout/audioTimeStretch.ts` のモジュール doc を参照)。
- `src/playout/stretch.rs` のモジュール doc は「時間圧縮・時間伸長」「削ったり挿したり」の
  ままで、時間伸長を持たない実装と食い違っている。
- `src/playout/feedback.rs` の `AudioDelayFeedback::reset` は学習を初期状態へ戻す。購読の
  やり直しでは目標を戻さない設計であり (`PlayoutTimeline::reset_track` と
  `PlayoutTimeline::reset` は閉ループを残す)、`src/` と `examples/` に呼び出しが無く、
  テストだけが使っている。

## 設計方針

- `expand` と `AudioDelayFeedback::reset` を削除する。残すと、未使用のまま「使える API」と
  して利用者に見え、テストだけが維持するコードになる。
- `stretch.rs` のモジュール doc から時間伸長の記述を外し、持たない理由 (この再生は音を
  伸ばす場面を持たず、足りない区間は補間が埋める) を書く。
- テストは削除に追随させ、削除で失われる検証を別の API へ置き換えることはしない。
  `reset` が担っていた「学習のやり直し」は `new()` での作り直しで代替できるが、初期状態の
  契約 (目標が初期値であること、理由が初期状態であること、観測が無いこと、観測が無い間は
  揺らぎだけの目標を上限で切って使うこと) は `tests/test_playout/feedback.rs` の
  `uses_the_jitter_target_until_the_first_observation` が `new()` 直後について既に確かめて
  いるため、pbt へ重複して書かず、テストごと削除する。
- ソースコードに issue 番号を書かない。

## 完了条件

- `expand` と `AudioDelayFeedback::reset` が `src/` から消え、`src/` と `examples/` に
  呼び出しが無いこと。
- `stretch.rs` のモジュール doc から時間伸長の記述が消え、理由が書かれていること。
- テストが削除に追随し、`make test` / `make clippy` / `make fmt` / `make pbt` が成功する
  こと。

## 解決方法

- `src/playout/stretch.rs` から `expand` を削除した (doc 13 行 + 本文 37 行 = 50 行)。
  モジュール doc を「時間圧縮と、欠落した区間の補間」に改め、時間伸長を持たない理由 (この
  再生は音を伸ばす場面を持たず、足りない区間は補間が埋める) を書いた。
  `is_time_stretch_available` と `conceal` の doc、および private 関数のコメントから
  `expand` への言及を外した。
- `src/playout/feedback.rs` から `AudioDelayFeedback::reset` を削除した (doc 5 行 + 本文
  16 行 = 21 行)。不要になった private の状態初期化は無く、`new()` の初期化はそのまま
  残した。明示設定の上限が学習のやり直しで残る、という性質は `reset` と一緒に消える
  (作り直す呼び出し側が `set_ceiling_us` で渡し直す)。
- `tests/test_playout/stretch.rs` から `expand` のテスト 7 本を削除した。`expand` を併せて
  使っていた 4 本 (`noise_is_not_stretched`、`silence_is_stretched`、
  `supported_sample_rates_are_stretched`、
  `is_time_stretch_available_accepts_only_the_downsample_table_rates`) は `compress` だけを
  確かめる形にした。判定と実挙動の一致は `compress` と `conceal` で確かめている。
- `pbt/tests/prop_playout/stretch.rs` から `expand` の property を削除した。
- `tests/test_playout/feedback.rs` から `reset` のテストを削除し、
  `pbt/tests/prop_playout/feedback.rs` から `reset` の property と、操作列の重みにあった
  `reset` の操作を削除した。
- 検証: `make fmt` / `make clippy` / `make test` / `make pbt` がいずれも exit code 0 で
  成功した。`rg "expand" src/ examples/` と `rg "expand" tests/ pbt/` は一致なし、
  `AudioDelayFeedback::reset` の呼び出しも残っていない (`rg "\.reset\(\)" src/ examples/`
  に残るのは decoder や `PlayoutTimeline` など別の型のものである)。
