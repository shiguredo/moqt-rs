# moq-sub の保留ストリームが自分の SUBGROUP_HEADER の受理を確認してから処理を進める

- Created: 2026-11-10
- Completed: 2026-11-10
- Branch: feature/fix-subscriber-pending-subscriber-notify
- Polished: 2026-11-10

## 目的

moq-sub の保留中の Subgroup ストリームの 1 本が購読の確立を検出すると、`note_subscriber(track_alias)` で同じ alias の全 entry に通知する。
通知を受けた別ストリームの task は、自分の `recv_subgroup_header` が一度も `Accepted` になっていないまま `true` を返して処理を進める。
Session 側ではそのストリームが header 未受理の状態のままであり、続く Object を `recv_subgroup_object` へ渡すと `MALFORMED_OBJECT` (0x201) のエラーが返る。各ストリームが自分の header の受理を確認してから処理を進めるようにする。

## 現状

- `crates/moq-sub/src/pipeline.rs` の `register_subgroup_header` は、保留ループの中で `data_plane.recv_subgroup_header(stream_id, header)` を再試行し、`Ok(TrackDataAcceptance::Accepted)` なら `note_subscriber(header.track_alias)` を呼んで次のループへ進む。
- ループの先頭は `take_ready_for(id, ..)` で自分の entry を引き取り、理由が `PendingNotifyReason::Subscriber` ならチャンクを decoder へ積み直して `true` を返す。
  同じ alias の別ストリームの task も、`note_subscriber` の通知によってループの先頭で `Subscriber` を引き取り、自分の `recv_subgroup_header` を `Accepted` にしないまま `true` を返す。
- `notify_alias` は同じ alias の全 entry (呼び出し元自身の entry を含む) を通知し、理由未設定の entry だけを更新する。検出元 task は、ループ再試行で `Accepted` を観測して `note_subscriber` を呼んだ直後の次のループ先頭で、自分の entry を `Subscriber` で引き取る。しかし現状の検出元 task は「自分の header が受理済みかどうか」を区別する手段を持っていない。
- Session 側 (`src/session/data.rs`) では当該 stream は `recv_subgroup_header` の受理で `IncomingDataStream::AwaitingHeader` から `Subgroup` に置換され、受理済みの stream への 2 度目の `recv_subgroup_header` は `SESSION_UNEXPECTED_MESSAGE` (0x303) の `Err` になる (キャンセル由来 Terminated に属する場合の例外を除く)。
- `take_ready_for` は取得時に entry を `taken` にしてチャンクの所有権を呼び出し側へ移し、未登録・削除済み・引き取り済みの entry に対しては `None` を返す。通知は「最初の 1 回だけ有効」であり、理由を上書きしない。削除済み・未登録の entry への `push` は no-op であり、`expire` による `Timeout` の確定は理由未設定の entry にしか行われない。そのため entry を一度引き取ったあとは、バッファ側から同じ entry で保留を続ける手段が存在しない。
- `register_subgroup_header` の doc は「この example は受信ループを始める前にすべての `SUBSCRIBE_OK` を処理するため、受信ループの実行中に購読が確立する経路は無く `note_subscriber` を呼ぶ箇所はない」とする。
  しかし同じ関数が再試行の `Accepted` で `note_subscriber` を呼んでいる。また受信ループの中には request 単位の GOAWAY から呼ばれる `reissue_request` があり、受信ループの実行中に購読が確立し得る。
- `src/session/data.rs` の `take_incoming_subgroup_object` は、header 未受理 (`IncomingDataStream::AwaitingHeader`) の stream への Object を `SESSION_MALFORMED_OBJECT` (0x201) で拒否する。
- `src/pending_subgroup_buffer.rs` の `note_subscriber` は alias 単位の通知であり、同じ alias の全 entry を引き取り待ちにする (購読の確立は alias 単位の事象であるため、ライブラリ側の契約は正しい)。

## 設計方針

- 検出元 (自分のループ再試行で `Accepted` を観測した task) と検出先 (別 task の `note_subscriber` によって通知され `Subscriber` を引き取った task) を区別する。検出元は、`Accepted` を観測したときに task ローカルのフラグ (検出元フラグ) を立て、次のループ先頭で自分の entry を `Subscriber` で引き取ったら再試行せずにチャンクを decoder へ戻して `true` を返す (現行の挙動を維持する)。
  検出先のみ、`true` を返す前に自分の `recv_subgroup_header` を再試行して `Accepted` を確認してからチャンクを戻す。同じ stream への 2 度目の `recv_subgroup_header` は `SESSION_UNEXPECTED_MESSAGE` (0x303) で拒否されるため、検出元での再試行は禁止する。
- 検出先の再試行結果は次のように処理する。
  - `Accepted` → チャンクを decoder へ戻して `true` を返す。この分岐で `note_subscriber` を呼ぶ必要は無い (各 task が自分の再試行で検出する。自分の entry は既に `Subscriber` で引き取り済みであり、他の entry への再通知も no-op であるため衝突は起こらない)。
  - `UnknownTrackAlias` → 保留を続ける。entry は引き取り済み・削除済みであり、`take_ready_for` による `Timeout` 確定も二度と起こらないため、保持チャンクと継続読み出しのチャンクを task ローカルで保持する。期限は保持の開始時刻 (`added_us`) からの `timeout_us` (`deadline_us = added_us + timeout_us`) を task 側で判定し、期限を延長しない (entry の再追加は期限を `now + timeout_us` に延ばすため行わない)。
    継続中も stream は読み続ける (既存の保留ループと同じ形で `wait_data_stream_stop_sending` の監視と `recv_subgroup_header` の再試行を続ける)。読み出しが終端 (`End` / Session の STOP_SENDING 指示 = `StoppedBySession`) を観測したら、終端種別を task ローカルで記録して読み出しを止める (終端済み stream は FIN を即時に再配信するため読み出しを続けると busy loop になる。
    Session への通知は既存の読み出しヘルパがそのまま行い、本 issue では Session 側の契約を変えない)。終端観測後の本 issue 時点の扱いは RESET / STOP_SENDING 指示と同じ破棄である (FIN 済み保持は 0425 が Session 通知の遅延とともに導入するため、ここで先取りしない)。終端を観測したまま期限に達した場合、既存の `Timeout` 分岐と同じ warn ログを出して破棄する。この経路のチャンクはバッファに積まれないため、per-session 上限の集計からは外れる。
    この時間窓は自分の `recv_subgroup_header` の再試行で早く解消する短期間であり、受入れられる制約としてその旨を doc に明記する。
  - `Discarded` / `FilteredOut` → 既存の再試行分岐と同じく `abort_subgroup_stream` を呼んで `false` を返す (entry は既に削除済みのため `remove` は不要である)。
  - `Err` → 既存の再試行分岐と同じく abort を呼ばずに `false` を返す。
- ライブラリの `PendingSubgroupBuffer::note_subscriber` は変更しない。
- `register_subgroup_header` の doc の「受信ループの実行中に購読が確立する経路は無く」を実態に合わせる。
- 検出元・検出先の区別と上記の処理分類は、0425 の FIN 済み保持の `Subscriber` 引き取りでも同じ規則として使うため、doc に分けられる形で書く。

## 変更対象

- `crates/moq-sub/src/pipeline.rs` (`register_subgroup_header` の検出元・検出先の区別、検出先再試行の処理分類、doc 修正)
- 同一ファイルの `mod tests` (処理分類の純関数テスト)
- Session 側 (`src/session/data.rs`) と `src/pending_subgroup_buffer.rs` は変更しない

## 完了条件

- 検出先の保留ストリームが、自分の `recv_subgroup_header` が `Accepted` になる前に decoder へチャンクを渡して処理を進めないこと。
- 検出元 (自分の再試行で `Accepted` を観測した task) の解放経路が現行と同一であること (受理済み stream への再試行が発生せず、`SESSION_UNEXPECTED_MESSAGE` (0x303) による確立済み stream の誤破棄が起こらないこと)。
- 同じ alias の複数ストリームが保留中に購読が確立した場合、各ストリームがそれぞれ自分の header の受理を確認してから処理を進めること。`note_subscriber` の重複通知が理由を上書きしない契約の下で誤動作しないこと。
- 検出先の再試行が `UnknownTrackAlias` で保留を続ける場合、期限は保持の開始時刻からの `timeout_us` で延長されないこと。読み出しが終端 (`End` / `StoppedBySession`) を観測したら終端種別を task ローカルで記録して読み出しを止め、busy loop が作られないこと。期限に達した場合は既存の `Timeout` 分岐相当の打ち切りになること。この経路のチャンクが per-session 上限の集計から外れる制約が doc に明記されていること。
- 検出先の処理分類 (検出元フラグ × 再試行結果から復帰 / 保留継続 / abort / 破棄を返す判定) は純関数に切り出し、`crates/moq-sub/src/pipeline.rs` の `mod tests` の単体テストで固定すること (0425 と同等の基準)。経路の駆動は `transport::RecvStream` と `DataPlaneHandle` を組み立てる基盤が無く、モック・スタブも使えない (AGENTS.md が禁止) ためコード上で確認すること。
- `register_subgroup_header` の doc が検出元・検出先の区別と検出先再試行の処理分類を説明していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `crates/moq-sub/src/pipeline.rs` の `register_subgroup_header` を検出元 (自分のループ再試行で `Accepted` を観測した task) と検出先 (別 task の `note_subscriber` で起きた task) に区別して整理した。検出元は entry を `Subscriber` で引き取ったら再試行せずに復帰し、検出先は自分の `recv_subgroup_header` を再試行して `Accepted` を確認してから復帰する。
  session は受理済みの stream への 2 度目の SUBGROUP_HEADER を `SESSION_UNEXPECTED_MESSAGE` (0x303) で拒否するため、検出元での再試行は発生しない。
- 検出先の再試行結果の処理分類 (復帰 / 保留継続 / 会計を閉じる破棄 / 会計を閉じない破棄) を純関数 `pending_subscriber_action` に切り出した。`Discarded` / `FilteredOut` は既存の再試行分岐と同じ `abort_subgroup_stream` で会計を閉じ、エラーは既存の `Err` 分岐と同じく会計を閉じずに破棄する。
- `UnknownTrackAlias` での保留継続は task ローカル保持に替えた (`LocalPendingHold` と `continue_pending_subscriber_locally`)。entry は引き取りの直後に削除されるため、チャンクと期限 (`deadline_us = added_us + timeout_us`) を task 内で持ち、バッファに戻さない (期限延長を行わない)。
  読み出しは `receive_registered_stream_data_outcome` を直用して終端種別を区別し、終端を task 内で記録して読み出しを止める。RESET_STREAM / Session の STOP_SENDING 指示は既存の終端分岐相当で破棄し、FIN は期限まで待ってから既存の Timeout 分岐相当で破棄する (FIN 済み保持の保持解除は後続の FIN 保持対応で導入する)。
- per-stream 上限は task 側で適用し、超過時は既存の overflow 分岐と同じ警告で破棄する。per-session 上限の集計から外れる制約を `register_subgroup_header` と `continue_pending_subscriber_locally` の doc に明記した。doc の「受信ループの実行中に購読が確立する経路は無く」の記述を実態に合わせて削除した。
- テスト: `pipeline::tests::pending_subscriber_action_tests` に検出元の無条件復帰、検出先の `Accepted` / `UnknownTrackAlias` / `Discarded` / `FilteredOut` / エラー / 再試行なしの 6 分岐を固定した。
- Session 側 (`src/session/data.rs`) と `src/pending_subgroup_buffer.rs` は変更しない (完了条件のとおり)。
