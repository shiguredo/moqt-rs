# publisher が SUBSCRIBE_OK に LARGEST_OBJECT を載せない場合がある

- Created: 2026-10-15
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-largest-object-in-subscribe-ok
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §9.20.17 (LARGEST OBJECT Parameter) は "If Objects have been
published on this Track the Publisher MUST include this parameter." と定める。しかし自側が
publisher 役のとき、ある Track の**最初の購読者**への SUBSCRIBE_OK には LARGEST_OBJECT が
載らない。購読者はこの値で FETCH の range を決めるため (moq-sub の `plan_fetch_range` は
`largest: Option<Location>` が `None` だと FETCH を発行しない)、最初の購読者だけが購読より
前の Group を取得できない。MUST を満たす。

## 現状

- `Session::send_subscribe_ok` (`src/session/subscription/send.rs`) は、アプリが渡した parameters に対して `update_largest_object_in_parameters` で LARGEST_OBJECT を補完する。補完に使う値は `Session::publisher_track_largest` が返す値だけである。
- `Session::publisher_track_largest` (`src/session/subscription/delivery.rs`) は「同一 Track の
  publisher 役 subscription の `effective_largest_object` の最大」を返す。publisher 役
  subscription の値は `largest_received_location` (自端点が**その購読へ送った** Object の最大
  Location) と `largest_location` (peer が広告した値) の max である (`src/session/types.rs`)。
- したがって、ある Track の最初の購読者への SUBSCRIBE_OK では「まだ 1 つも送っていない」ため補完する値が無く、パラメータが載らない。Object を publish 済みでも同じである。
- `examples/moq-pub` は `send_subscribe_ok` に `MessageParameters::new()` を渡しており、自分の保持する最新 Location を渡す経路も無い。
- 実測 (publisher を 5 秒先行させ、publisher と relay の組に対して購読): 1 人目の購読では
  `Skipping FETCH for the Video track: SUBSCRIBE_OK carried no Largest Object` となり FETCH が
  発行されなかった。同じ relay への 2 人目では `Sent FETCH for the recent groups:
  kind=Video, request_id=6, start=(..., 0), end=(..., 14)` となり発行できた。relay は
  「上流 SUBSCRIBE_OK の LARGEST_OBJECT を初期値にし、上流 subscription で受信した Object の
  最大で更新した値」を下流に載せるため、1 人目では値が無く、2 人目では受信済みの値が載る。
- 影響: FETCH が最も必要な「配信中の Track に最初に参加する購読者」で FETCH が使えない。

## 設計方針

- publisher 側で **Track 単位の publish 済み最大 Location** を保持し、`send_subscribe_ok` の補完に使う。subscription の送信実績に依存しない。
- 補完は既存の `update_largest_object_in_parameters` を使い、アプリが明示した値より大きい場合だけ置き換える (アプリ指定値を上書きしない現在の方針を維持する)。
- Object をまだ 1 つも publish していない Track では載せない (§9.20.17 の "If omitted from a message, the sending endpoint has not published or received any Objects in the Track." に合わせる)。
- `examples/moq-pub` が自分の保持する最新 Location を parameters で渡す経路を併せて用意するかは、ライブラリ側の補完で足りるなら不要とする。どちらを採るかを実装時に決めて doc に残す。

## 完了条件

- publish 済みの Track に対する最初の SUBSCRIBE への SUBSCRIBE_OK に LARGEST_OBJECT が載ること (ライブラリのユニットテストで固定する)。
- Object を publish していない Track では LARGEST_OBJECT が載らないこと。
- アプリが明示した LARGEST_OBJECT と publish 済み最大の大きい方が載ること。
- 2 人目以降の購読者の値が変わらないこと (既存の挙動を壊さない)。
- moq-sub が最初の購読者でもメディアの FETCH を発行できること (実 relay の E2E で確認する)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
