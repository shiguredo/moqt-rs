# E2E テストが一時ディレクトリを奪い合って MP4 を消すのを直す

- Created: 2026-10-14
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-e2e-work-dir-collision
- Polished: {YYYY-MM-DD}

## 目的

E2E Test が 2026-10-14 の 0253 以降、毎回失敗している。原因は、E2E テストが増えたことで
同じプロセスの中で 2 つのテストが並列に走り、`WorkDir` が同じ一時ディレクトリを奪い合う
ようになったことである。

`WorkDir::new()` は `std::env::temp_dir().join(format!("moqt-e2e-{}", std::process::id()))`
を作る。`cargo test` はテストをスレッドで並列に実行するため、2 つのテストが同じプロセス ID を
名前に入れ、互いのディレクトリを削除してから作り直す。先に始めたテストが受信中の MP4 を
書いている最中に、もう片方のテストが `remove_dir_all` でディレクトリごと消す。

失敗の現れ方は 2 つある。

- `publish_and_subscribe_roundtrip`: 映像フレームは届くのに `MP4 ファイルが作成されて
  いないこと` で落ちる。書いた MP4 がもう片方のテストに消されている
- `publish_and_subscribe_audio_datagram_roundtrip`: publisher が
  `datagram send error: Queue does not have room for more datagrams.` で終了し、
  subscriber がフレームを受信できない。購読側が録画の書き込みで止まり、datagram を
  受け取らなくなった結果、送信のキューが溢れる

どちらも実装ではなくテストの作りが原因である。

## 現状

- `e2e-tests/tests/roundtrip.rs` の `WorkDir::new()` は引数を取らず、プロセス ID だけで
  名前を作る
- テストは `publish_and_subscribe_roundtrip` と
  `publish_and_subscribe_audio_datagram_roundtrip` の 2 つがあり、どちらも `WorkDir::new()`
  を呼ぶ
- 0253 でテストを 2 つに増やした時点から失敗が始まった (0253 より前は成功していた)

## 設計方針

- `WorkDir::new(name)` にして、テストごとに一意な名前を渡す
- 名前はテストの目的を表す短い語にする
- 実行が前回の残骸を使い回さないよう、いったん消してから作る挙動は残す
- 並列に走るテストが互いの成果物を消さないことをテストで固定する

## 完了条件

- 同じプロセスで 2 つのテストが並列に走っても、互いのディレクトリと成果物を消さないことが
  テストで固定されていること
- E2E Test が成功すること
- `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

(対応時に記入)
