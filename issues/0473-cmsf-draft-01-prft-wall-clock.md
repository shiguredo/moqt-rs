# cmaf の prft で音声と映像を壁時計の時間軸に載せる

- Created: 2026-11-23
- Completed: {YYYY-MM-DD}
- Branch: feature/add-cmsf-draft-01
- Polished: 2026-11-23

## 目的

moq-sub は、壁時計の Timestamp を持つフレームだけを時間軸に載せる。それ以外の映像フレームは届いた順に表示し、音声は PTS の順に並べるだけで時間軸には観測しない。0471 / 0472 の cmaf では、時刻が tfdt (メディア時刻) で伝わるので `TimestampKind::MediaTime` になる。その結果、live capture でも音声と映像の同期と遅延の追従が働かない。

0471 の live capture では、tfdt はすでに Unix epoch マイクロ秒 (timescale 1,000,000) である。しかし CMAF には LOC の「Timescale がない」に当たる印がないので、moq-sub は tfdt が壁時計かどうかを判別できない。

本 issue では、live capture の cmaf の chunk に prft (Producer Reference Time Box) を付けて、media time と壁時計の対応を伝える。moq-sub はそれを読んで、LOC の壁時計 Timestamp と同じ時間軸に載せる。

前提は 0471 (moq-pub) と 0472 (moq-sub) である。CMSF 対応全体の方針は 0467 を参照すること。

## 現状

- `crates/moq-sub/src/main.rs` の `wall_clock_timestamp` / `wall_clock_pts` は、`TimestampKind::WallClock` のときだけ時刻を返す。
- `crates/moq-sub/src/pipeline.rs` の `receive_input_lag_us` (受信の入口で測る入力の遅れ):
  - 引数は LOC の Timestamp / Timescale、Object の到着時刻、PTS である。Timestamp があり、Timescale が無いか 0 のときだけ値を返す。
  - Timescale 0 を壁時計として測るのは closed 0292 で決めた挙動で、テスト `receive_input_lag_us_measures_the_wall_clock_timestamp` が固定している。一方、`timestamp_kind_of` は Timescale 0 も `MediaTime` にする (`timestamp_kind_of_returns_media_time_with_a_timescale`)。
  - 呼び出し元は `AudioDelivery::deliver` の 1 か所である。端末間遅延の debug ログ (`log_audio_end_to_end_lag`) は、`receive_input_lag_us` の返り値が `Some` のときだけ出る。
  - `receive_input_lag_us` の doc コメントは `log_audio_end_to_end_lag` の doc の前に置かれている。そのため rustdoc 上は `log_audio_end_to_end_lag` に付いている。
- `shiguredo_mp4` (2026.6.0-canary.2) には prft の型がない。`Fmp4SegmentDemuxer` は moof の前の prft を解釈せずに読み飛ばす。
  - 同 crate のコメントは、prft を ISO/IEC 14496-12:2022 の 8.16.5 としている。
  - 上流の shiguredo/mp4-rs の `issues/0071-other-position-timing-boxes-inventory.md` は、prft を「要求が出るまで見送り」としている。
- ISO/IEC 14496-12 と CMAF 本体 (ISO/IEC 23000-19) の本文は `refs/` にない。
- MSF-02 §7 の Media Timeline について:
  - ライブラリには `MsfMediaTimeline` / `decode_media_timeline` がある。
  - moq-pub / moq-sub は、Media Timeline track の発行も購読もしていない。

## 設計方針

### prft を使う理由

- MSF-02 §7 の Media Timeline track も、media time と壁時計の対応を伝える (「The media timeline track provides data about the previously published Groups and their relationship to wallclock time and media time」)。本 issue では、これを使わずに prft を使う。理由は次のとおりである。
  - Media Timeline は、すでに publish された Group を記述する別の track なので、ライブの先頭の Object より後に届き得る。media PTS は整数ミリ秒 (§7.1.1) で、Group 単位である。
  - prft は Object payload の中で chunk ごとに対応を運ぶので、最初の Object から使える。
- Media Timeline との併用は範囲外とする。

### 着手前の確認

- 規格本文が `refs/` にないため、実装に着手する前に次の 2 つを本文で確認する。
  - ISO/IEC 14496-12 (`shiguredo_mp4` のコメントが参照する 2022 年版) §8.16.5: prft の構造。具体的には、version ごとの media_time の幅、media_time の単位 (reference track の mdhd の timescale であること)、reference_track_ID、ntp_timestamp の形式、flags の値と意味である。
  - ISO/IEC 23000-19 (CMAF): CMAF Chunk の moof の前に prft を置けること。CMSF §3.3 は Object を CMAF Chunk と同一視しているので (「This is equivalent to requiring that each Object hold at least one CMAF Chunk」)、CMSF の本文だけでは決まらない。
- どちらかを確認できない場合は着手しない。shiguredo-issues の規約に従い、理由を追記して pending にする。0474 は開始条件に 0473 を含めていないので、0473 が pending になっても 0474 は進められる。
- 本文で確認できても、この issue の設計の前提と合わない場合は、着手せずに、確認した内容を issue に追記して設計を見直す (見直した issue を `/polish-issue` にかけてから着手する)。前提は次のとおりである。
  - CMAF Chunk ごとに、moof の前に prft を置けること
  - ntp_timestamp が capture の時刻であることを表す flags の値があること
  - media_time を 64 bit で書ける版があること
  - media_time の単位が reference track の mdhd の timescale であること
  - ntp_timestamp が 32 bit の秒と 32 bit の小数部の NTP 形式であること
  - ntp_timestamp が UTC に揃った時刻であることを表す flags の値が、本文から決まること。受信側は、そう確かめた値の prft だけを受け入れ、それ以外 (UTC に揃わない値と、本文が意味を定めない値) の prft を「不正」として扱う (下の moq-sub の節)
  - capture の時刻を表す flags の値が、UTC に揃った時刻を表す値に含まれること。含まれないと、moq-sub が moq-pub 自身の prft を「不正」として捨てる
- 確認した構造は、moq-codec のテストの固定バイト列の根拠として、版と節番号とともにテストのコメントに書く。

### 時刻の変換

- 壁時計は、moq-sub の時間軸に合わせて Unix epoch マイクロ秒で扱う。MSF-02 §11.6 の Wallclock time はミリ秒、LOC の Timestamp はマイクロ秒、prft の ntp_timestamp は NTP 形式 (32 bit の秒と 32 bit の小数部) である。
- マイクロ秒から NTP 形式への変換 (moq-pub):
  - 秒 = (epoch_us / 1,000,000 + 2,208,988,800) mod 2^32。2,208,988,800 は 1900-01-01 から 1970-01-01 までの秒数 ((70 × 365 + 17) × 86,400) である。
  - 小数部 = floor((epoch_us mod 1,000,000) × 2^32 / 1,000,000)。中間値は最大で約 4.29e15 なので u64 に収まる。
- NTP 形式からマイクロ秒への変換 (moq-sub):
  - 秒の最上位 bit が 1 なら 1900-01-01 起点、0 なら 2036-02-07T06:28:16Z (1900-01-01 の 2^32 秒後、Unix 2,085,978,496 秒) 起点として読む (実装判断)。32 bit の秒はこの時刻で一周するためである。これで 1968〜2104 年を表せる。
  - 秒未満のマイクロ秒 = (小数部 × 1,000,000 + 2^31) >> 32 (四捨五入)。
    - 組み立てを切り捨て、解析を四捨五入にすると、秒未満のすべての値 (0〜999,999 µs) で往復が一致する。両方を切り捨てにすると一致しない (例: 1 µs → 4,294 → 0)。
    - 四捨五入の結果は 1,000,000 になり得るので、桁を連結せずに秒のマイクロ秒へ加算する。
- prft の対応による sample の時刻の換算 (moq-sub が使う。置き場所は「コードの置き場所」のとおり moq-codec):
  - wall_us = ntp_us + (pts − media_time) × 1,000,000 / timescale
  - pts と media_time は track の mdhd の timescale (0472 が init segment から保持する値) の単位である。差は符号付きで求め、中間計算は 128 bit で行い、i64 で飽和させる。0472 の DTS の換算と同じ幅である。
  - 除算は床関数 (負の無限大方向への丸め、`div_euclid`) とする。0 方向への切り捨てにすると、差の正負で丸めの向きが変わる (例: 48 kHz の −1 tick は −20.83 µs で、床関数なら −21、0 方向なら −20)。
  - mdhd の timescale は `shiguredo_mp4` の `MdhdBox::timescale` が `NonZeroU32` で、0 の init segment は 0472 の decode で起動の失敗になる。換算は `NonZeroU32` の timescale を受け取る。
  - 0472 は composition offset が 0 でない track を対応外にするので、pts は DTS と同じ値になる。

### moq-pub (live capture の cmaf だけ)

- 各 chunk の moof の前に prft を 1 つ置く。`--input-mp4` / `--input-mp4-reencode` には壁時計の capture 時刻がないので、prft を付けない。
- prft の値:
  - reference_track_ID: chunk の track_ID (0471 では 1)。
  - media_time: その chunk の tfdt。live capture の tfdt (epoch µs、約 1.8e15) は u32 に収まらないので、media_time を 64 bit で書く版を使う。
  - ntp_timestamp: tfdt (epoch µs) を上の規則で NTP 形式にした値。
    - tfdt は 0471 のとおり LOC 経路の Timestamp で、逆行を防いだ chunk だけ「直前の tfdt + 1」になる。
    - 補正前の Timestamp ではなく tfdt を使うのは、moq-sub で換算した時刻を、tfdt と同じく track ごとに逆行させないためである。補正前の値を書くと、換算後の PTS が直前の chunk より小さくなり、`AudioDeliveryOrder::admit` がその音を捨てる。
  - flags: ntp_timestamp が capture の時刻であることを表す値を、着手前の確認で決めて使う。
  - 結果として、prft の対応は media_time と壁時計の恒等な対応になる。prft が運ぶのは「この track の tfdt は壁時計である」という印である。
- prft は chunk のバイト列の一部なので、0471 の「chunk のバイト列は、1 sample につき 1 回だけ組み立てる」に含める。
  - track ごとの chunk の組み立てで prft + moof + mdat を作る。そのバイト列を、PUBLISH の writer、受信 SUBSCRIBE の購読者 (`send_video_to_peer_subscriptions` / `send_audio_to_peer_subscriptions`)、`media_cache` (FETCH) で同じものとして使う。
  - ntp_timestamp は、送出時の現在時刻ではなく tfdt から決める。そのため、どの経路でも同じ値になる。
  - 終了時のフェード (`flush_audio_fade`) の chunk にも、同じ組み立てで prft を付ける。
  - 経路ごとに組み立て直すと payload が変わり、MoQT-23 §7.1 の「An endpoint that receives a duplicate Object with a different Delivery Mode, Subgroup ID, Priority or Payload MUST treat the track as Malformed」に反する。
  - LOC 経路で受信 SUBSCRIBE の購読者向けに組み立て直せるのは、時刻が Object Properties にあり、§7.1 が「Only Object Properties can be added, removed or updated」としているためである。

### moq-sub

- prft の読み取りは、0472 の分割と検証の関数で行う。
  - 0472 は、moof の前にある box を直後の組に含めて渡す。組の中で moof より前にある prft を、その組の moof の sample に対応づける。prft と moof の間に styp などの他の box があってもよい。1 つの組に「不正」でない prft が複数ある場合は、最後のものを使う (実装判断)。
  - 分割と検証の関数は、組ごとの prft の解析結果 (「prft なし」「対応」「不正」のいずれか) を sample の列と一緒に返すだけで、状態の参照も時刻の換算もしない。I/O を持たない関数のまま、Object 単位で呼べるようにするためである。
  - 次の prft は「不正」として返す。呼び出し側は英語の debug ログを出し、prft がないものとして扱う。media の復号には影響しないので、0472 の「Object を破棄する」「track を対応外にする」には当てない。
    - 長さや version が §8.16.5 の構造に合わない
    - reference_track_ID が、init segment の media trak の track_ID と一致しない。CMSF §3.3 は「MUST contain a single track」としているので、他の track を指す prft は意味を持たない
    - flags の値が、着手前の確認で「ntp_timestamp が UTC に揃った時刻である」と確かめた値でない。本文が意味を定めない値もここに含める。prft は「この track の tfdt は壁時計である」という印として使うので、UTC に揃わない時刻を運ぶ prft は印にならない
- 対応の引き継ぎ:
  - track ごとに、読んだ prft のうち media_time が最も大きいものの対応 (media_time と壁時計) を保持する。prft のある組はその組の prft で、prft のない組は保持している対応で換算し、`TimestampKind::WallClock` にする。対応をまだ持たない track の組だけを、0472 のとおり `TimestampKind::MediaTime` にする。
  - 状態を更新するのは、対応をまだ持っていないときと、読んだ prft の media_time が保持している対応の media_time より大きいときだけである。FETCH の応答や重複した Object のように後から届く古い Object の prft で、保持している対応が古いものに戻らないようにするためである。対応が chunk ごとに変わる publisher (時計の drift の補正など) でも、保持している対応は新しい方向にしか動かない。
  - 状態の更新と換算は、cmaf の入口の前段 (0472 の対応外の状態を確かめる呼び出し側) で行う。
    - 分割と検証の関数が sample の列を返した Object だけを対象にする。「Object を破棄する」「track を対応外にする」の結果になった Object の prft は使わない。
    - 対象の Object では、組の順に処理する。prft のある組はその prft で換算し、上の規則に当たれば状態を更新する。prft のない組は、その時点の状態で換算する。1 つの Object の中で「prft のない組 → prft のある組」と並ぶ場合も、組の順に処理する。
    - 換算した時刻と `TimestampKind` は sample ごとに持たせ、後段 (重複・購読境界・復号順の判定、復号、送出、録画) にはそれを渡す。同じ Object の中でも、組ごとに WallClock と MediaTime が分かれ得るためである。
    - 重複や購読境界で後段が落とす Object の prft も、この規則で状態の更新に使ってよい。重複した Object は payload が同じ (MoQT-23 §7.1) なので、同じ prft をすでに読んでいる。購読境界より前の Object は Location が前なので、track ごとに逆行しない tfdt と同じく media_time も小さい。そのため、より新しい対応を保持した後では状態を更新しない。
    - 最初に読む prft がどの Object のものになるかは到着順で決まる。それでも、対応をまだ持たない track では最初に読んだ prft で状態が作られ、以後は media_time の大きい prft でだけ置き換わるので、MediaTime から WallClock への切り替えは到着順によらず 1 回だけである。最初の prft より前に読んだ prft のない組は MediaTime になる。prft のない組の換算に使う対応は、その組を処理した時点の状態で決まる。
  - 引き継ぐ理由: Object や組ごとに判定すると、Group の先頭にだけ prft を置く publisher で、同じ track の中に WallClock と MediaTime が混ざる。
    - 映像では、時間軸に載るフレームと届いた順に出るフレームが混ざる。closed 0332 が直したのと同じ状態である。
    - 音声では、`AudioDeliveryOrder::admit` が種類を見ずに PTS で並べる。そのため、epoch µs の音の後に来たメディア時刻の音がすべて Stale になる。
  - 引き継ぐので、WallClock から MediaTime に戻ることはない。MediaTime から WallClock への切り替えは、その track で最初の prft を読んだときの 1 回だけである。
  - 対応の状態は、0472 の対応外の状態と同じく I/O を持たない専用の型にする。同じ置き方と配り方で track 単位で共有し、音声は 4 か所、映像は `DatagramTaskContext` を除く 3 か所に配る。映像は Group ごとに別の stream task になり、音声は datagram と subgroup の両方から届くためである。
- 他の publisher の prft も、「不正」でなければ壁時計として受け入れる。つまり、flags が UTC に揃った時刻を表すと確かめた値の prft を、それ以上の条件なしに受け入れる (実装判断)。publisher の時計が UTC からずれている可能性は、LOC の Timescale がない Timestamp と同じく受信側では判定しない。
- 換算した時刻を `DecodedVideoFrame::timestamp_us` と音声の PTS に載せ、`TimestampKind::WallClock` にする。これで `wall_clock_timestamp` / `wall_clock_pts` が時刻を返し、既存の時間軸に載る。
- 入力遅れの計測:
  - `receive_input_lag_us` の引数を、壁時計の時刻 (`Option<i64>`、壁時計でなければ `None`) と到着時刻にする。`TimestampKind` は渡さない。`TimestampKind` で判定すると、LOC の Timescale 0 の音が `MediaTime` になり、0292 で決めた計測が止まるためである。
  - LOC 経路の規則 (Timestamp があり、Timescale が無いか 0 なら、PTS が壁時計の時刻) は、I/O を持たない補助関数 `loc_wall_clock_us` (Timestamp、Timescale、PTS を取り、壁時計の時刻を `Option<i64>` で返す) に移す。LOC 経路はこの関数の返り値を `receive_input_lag_us` に渡す。LOC の挙動は変えない。
  - 既存のテスト `receive_input_lag_us_measures_the_wall_clock_timestamp` / `receive_input_lag_us_returns_none_when_the_timestamp_is_not_a_wall_clock` の各ケース (Timescale なし、Timescale 0、負の遅れ、Timestamp なし、Timescale あり) は、`receive_input_lag_us(loc_wall_clock_us(timestamp, timescale, pts), arrival)` の合成で書き直し、期待値を変えない。
    - 2 つの関数を別々に試すと、`loc_wall_clock_us` の期待値は遅れではなく壁時計の時刻になり、0292 で決めた Timescale 0 の遅れの値を固定するテストが残らないためである。
  - cmaf 経路は、prft の対応で換算した時刻を渡す。対応がなければ `None` を渡す。
  - これで、prft のある cmaf の音でも入力遅れが計測され、`log_audio_end_to_end_lag` も出る。
  - 引数を変えるときに、`receive_input_lag_us` の doc コメントを本体の直前に移す。

### コードの置き場所

- prft の組み立てと解析、NTP 形式との変換、prft の対応による sample の時刻の換算は `crates/moq-codec` に置く。換算は、解析した prft の対応の型のメソッドにする。
  - flags を受け入れるかの判定 (UTC に揃った時刻を表すと確かめた値の集合) と、moq-pub が書く capture の時刻を表す flags の値も、moq-codec に置く。moq-sub の分割と検証の関数は、この判定を使って「不正」を決める。送信側の値と受信側の判定を同じ crate に置くことで、moq-pub の prft が moq-sub で捨てられないことを moq-codec と moq-pub のテストで確かめられる。
  - moq-pub と moq-sub は互いに依存していないので、組み立てと解析を同じ固定のバイト列で確かめ、NTP 形式の変換の往復を全数で確かめるには、両方が依存する crate に置く必要がある。
  - 換算は moq-sub だけが使うが、prft の media_time と ntp_timestamp の意味 (単位と起点) に依存する。prft の意味を扱うコードを 1 か所にまとめるため、解析と同じ crate に置く。両方が依存するワークスペース内の crate のうち、メディアの形式を扱う補助を置くのは moq-codec である。ワークスペース内のほかの共通の依存は、tokio-moq がトランスポート層、happy-eyeballs が接続候補のスケジューラ、shiguredo_moqt が MOQT 本体で、box の補助を置く責務に合わない。
  - moq-codec の crate doc (「moq-pub / moq-sub の example が共有する codec デコーダ」) に、共有する box の補助も置くことを追記する。`crates/moq-codec/Cargo.toml` の description (「Shared codec decoders for the MOQT examples」) も、同じ内容に揃える。
- `shiguredo_mp4` への型の追加は範囲外とする。上流の mp4-rs の 0071 が待っている「要求」には当たるが、別リポジトリの作業である。

### 文書

- `docs/msf.md` の CMSF の節に、prft の使い方を追記する。内容は次のとおりである。
  - live capture だけに付けること
  - 値の決め方
  - 受信側で対応を引き継ぐこと
  - Media Timeline を使わない理由

## 範囲外

- MSF-02 §7 の Media Timeline との併用
- `shiguredo_mp4` への prft の型の追加
- LOC の Timescale 0 の扱いを `timestamp_kind_of` と `receive_input_lag_us` で揃えること (0292 から続く既存の挙動)
- `--input-mp4` / `--input-mp4-reencode` への prft の付与

## 完了条件

- moq-codec の単体テスト:
  - 着手前に確認した §8.16.5 の構造から手で書いた固定のバイト列と、組み立ての結果および解析の結果が一致すること。テストのコメントに版と節番号を書く。組み立てと解析の両方向をこの固定のバイト列で確かめるので、別の往復テストは置かない。
  - flags の判定が、capture の値を受け入れ、UTC に揃った時刻を表すと確かめた値でない値を「不正」とすること。
  - マイクロ秒と NTP 形式の変換が、秒未満のすべての値 (0〜999,999 µs) で往復一致すること。2036-02-07T06:28:16Z の前後の時刻を、正しい起点で読むこと。
  - prft の対応による換算が、差が負の場合 (床関数で丸めること) と飽和する場合を含めて、上の式のとおりになること。
- moq-pub:
  - live capture の cmaf の Object が prft + moof + mdat の形になり、`--input-mp4` / `--input-mp4-reencode` の Object には prft がないこと。
  - moq-pub が書いた prft の flags が moq-codec の capture の値で、その prft を moq-codec の解析と flags の判定に通すと「不正」にならないこと。moq-pub は moq-sub に依存しないので、moq-sub と共有する moq-codec の判定で確かめる。
  - prft の media_time が tfdt と一致すること。ntp_timestamp をマイクロ秒に戻した値が tfdt と一致すること。reference_track_ID が track_ID と一致すること。逆行を防いだ chunk でも同じであること。
  - 0471 の「同じ Location の media の Object のバイト列が、すべての経路で一致すること」が、prft を含むバイト列で成り立つこと。終了時のフェードの chunk も prft を持つこと。
- moq-sub:
  - 分割と検証の関数が、組ごとに解析した prft の対応を返すこと。不正な prft (長さ、version、reference_track_ID の不一致、UTC に揃った時刻を表すと確かめた値でない flags) は「不正」として返すこと。
  - 対応の状態の型に組の順で適用すると、prft のある組はその prft で、prft のない組は保持している対応で換算され、WallClock になること。対応を持つ前の組は MediaTime になること。1 つの Object に「prft のない組 → prft のある組」が並ぶ場合も、組の順に換算されること。保持している対応より media_time が小さい prft では、その組の換算には使うが、状態を更新しないこと。
  - prft のある cmaf の Object が `TimestampKind::WallClock` の時刻で配送されること。prft を一度も読んでいない track の Object は `TimestampKind::MediaTime` のままであること。分割と検証の関数、対応の状態の型、cmaf の入口を順に通すテストで確かめる。入力は、0472 の cmaf の入口のテストと同じく `shiguredo_mp4::boxes` で組み立てた実際のバイト列に、moq-codec で組み立てた prft を前に置いたものとする。
  - 0472 の 5 つの呼び出し元 (`decode_video_stream` / `read_video_fetch_stream` / `decode_audio_stream` / `deliver_datagram` / `run_side_session`) がすべて、cmaf の入口の前段で状態の更新と換算を行うこと。対応の状態が、音声は 4 か所、映像は `DatagramTaskContext` を除く 3 か所に、同じインスタンスとして配られていること。
    - 0472 と同じく配線の要件で、テストで経路ごとに観測しにくいので、コードレビューで確認する。`read_video_fetch_stream` で前段を漏らしても、起動時の FETCH の Object は購読境界以下で表示されないので (closed 0280)、E2E では気づけない。
  - prft のある cmaf の音で、入力遅れが catch-up に渡ること。既存の `audio_delivery_passes_the_input_lag_to_the_catch_up` と同じ形で、0472 の cmaf の入口を使って確かめる。
  - `receive_input_lag_us` の既存テストの各ケースが、`receive_input_lag_us(loc_wall_clock_us(timestamp, timescale, pts), arrival)` の合成で、期待値を変えずに通ること。
  - テストではモックを使わない。
- 0472 で追加した cmaf の E2E テストが、prft 付きの live capture で通ること。
- `docs/msf.md` の CMSF の節に、prft の使い方を追記すること。
- moq-codec の crate doc と `Cargo.toml` の description に、prft の組み立てと解析、flags の判定、NTP 形式の変換、換算を置くことが書かれていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
