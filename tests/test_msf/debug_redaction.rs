//! MSF の公開型の `Debug` が機密値を伏せることのテスト
//!
//! token (§5.2.37)、trackBaseKey (§5.2.41)、authInfo (§5.2.42。変数置換による token の混入は
//! §5.2.43)、token を含み得る URI
//! (connectionUri の §5.2.36、fragment パラメータの §11.1.1 / §11.4.2、query) の値を、
//! 文字列としても十進のバイト列としても出さず、長さだけを表示することを固定する。

use super::*;
use shiguredo_moqt::msf::uri::parse_msf_uri;

/// テストで使う bearer token のダミー値
const TOKEN: &str = "dummy-track-token";
/// テストで使う trackBaseKey のダミー値
const TRACK_BASE_KEY: &str = "dummy-base-key";
/// テストで使う authInfo の scheme 固有値 (生 JSON) のダミー値
const AUTH_VALUE_RAW: &str = "\"dummy-auth-value\"";
/// テストで使う connectionUri のダミー値 (token パラメータを含む)
const CONNECTION_URI: &str = "moqt://relay.example.com/pub?token=dummy-connection-token";

/// `debug` に `secret` が文字列としても十進のバイト列としても含まれないことを確認する
fn assert_hidden(debug: &str, secret: &str, context: &str) {
    assert!(
        !debug.contains(secret),
        "{context}: 実値を文字列として Debug に出さないこと"
    );
    // derive(Debug) の `Vec<u8>` は `[34, 100, ...]` と十進で出るため、その並びも探す
    let bytes = format!("{:?}", secret.as_bytes());
    let decimal = &bytes[1..bytes.len() - 1];
    assert!(
        !debug.contains(decimal),
        "{context}: 実値を十進のバイト列として Debug に出さないこと"
    );
}

/// scheme 固有値に token を持つ authInfo を作る
fn secret_auth_info() -> MsfAuthInfo {
    MsfAuthInfo {
        scheme: "cat".to_string(),
        value_raw: AUTH_VALUE_RAW.as_bytes().to_vec(),
    }
}

/// 機密フィールドすべてに値を持つトラックを作る
fn secret_track(name: &str) -> MsfTrack {
    let mut track = MsfTrack::new(name.to_string(), MsfPackaging::Loc, true);
    track.codec = Some("avc1.42E01E".to_string());
    track.token = Some(TOKEN.to_string());
    track.track_base_key = Some(TRACK_BASE_KEY.to_string());
    track.connection_uri = Some(CONNECTION_URI.to_string());
    track.auth_info = Some(vec![secret_auth_info()]);
    track
}

/// 機密フィールドすべてに値を持つクローントラックを作る
fn secret_clone_track() -> MsfCloneTrack {
    let mut track = MsfCloneTrack::new("video-clone".to_string(), "video".to_string());
    track.codec = Some("avc1.640028".to_string());
    track.token = Some(TOKEN.to_string());
    track.track_base_key = Some(TRACK_BASE_KEY.to_string());
    track.connection_uri = Some(CONNECTION_URI.to_string());
    track.auth_info = Some(vec![secret_auth_info()]);
    track
}

/// トラックの全機密値が伏せられ、長さと非機密フィールドが表示されることを確認する
fn assert_track_redacted(debug: &str, context: &str) {
    assert_hidden(debug, TOKEN, context);
    assert_hidden(debug, TRACK_BASE_KEY, context);
    assert_hidden(debug, CONNECTION_URI, context);
    assert_hidden(debug, "dummy-auth-value", context);
    assert!(
        !debug.contains("value_raw: ["),
        "{context}: authInfo の生 JSON をバイト列として出さないこと"
    );
    // 伏せたフィールドは長さを表示する
    assert!(
        debug.contains(&format!("token: Some({})", TOKEN.len())),
        "{context}: token の長さを表示すること: {debug}"
    );
    assert!(
        debug.contains(&format!("track_base_key: Some({})", TRACK_BASE_KEY.len())),
        "{context}: track_base_key の長さを表示すること: {debug}"
    );
    assert!(
        debug.contains(&format!("connection_uri: Some({})", CONNECTION_URI.len())),
        "{context}: connection_uri の長さを表示すること: {debug}"
    );
    assert!(
        debug.contains(&format!("value_raw_bytes: {}", AUTH_VALUE_RAW.len())),
        "{context}: authInfo の値の長さを表示すること: {debug}"
    );
    // 非機密フィールドは従来どおり表示する
    assert!(
        debug.contains("scheme: \"cat\""),
        "{context}: authInfo の scheme は表示すること: {debug}"
    );
}

/// `MsfTrack` の `Debug` が機密値を伏せ、非機密フィールドを表示すること
#[test]
fn track_debug_hides_secrets() {
    let debug = format!("{:?}", secret_track("video"));
    assert_track_redacted(&debug, "MsfTrack");
    assert!(
        debug.contains("name: \"video\""),
        "非機密の name は表示すること"
    );
    assert!(
        debug.contains("codec: Some(\"avc1.42E01E\")"),
        "非機密の codec は表示すること"
    );
    assert!(
        debug.contains("packaging: Loc"),
        "非機密の packaging は表示すること"
    );
}

/// 機密値が無いトラックの `Debug` は `None` を表示すること
#[test]
fn track_debug_shows_none_for_absent_secrets() {
    let track = MsfTrack::new("audio".to_string(), MsfPackaging::Loc, true);
    let debug = format!("{track:?}");
    assert!(
        debug.contains("token: None"),
        "token が無ければ None を表示すること"
    );
    assert!(
        debug.contains("track_base_key: None"),
        "track_base_key が無ければ None を表示すること"
    );
    assert!(
        debug.contains("connection_uri: None"),
        "connection_uri が無ければ None を表示すること"
    );
    assert!(
        debug.contains("auth_info: None"),
        "auth_info が無ければ None を表示すること"
    );
}

/// `MsfCloneTrack` の `Debug` が機密値を伏せ、非機密フィールドを表示すること
#[test]
fn clone_track_debug_hides_secrets() {
    let debug = format!("{:?}", secret_clone_track());
    assert_track_redacted(&debug, "MsfCloneTrack");
    assert!(
        debug.contains("parent_name: \"video\""),
        "非機密の parent_name は表示すること"
    );
    assert!(
        debug.contains("codec: Some(\"avc1.640028\")"),
        "非機密の codec は表示すること"
    );
}

/// `MsfAuthInfo` の `Debug` が値を伏せ、scheme と長さだけを表示すること
#[test]
fn auth_info_debug_hides_value_raw() {
    let info = MsfAuthInfo {
        scheme: "privacy-pass".to_string(),
        value_raw: AUTH_VALUE_RAW.as_bytes().to_vec(),
    };
    let debug = format!("{info:?}");
    assert_hidden(&debug, "dummy-auth-value", "MsfAuthInfo");
    assert_eq!(
        debug,
        format!(
            "MsfAuthInfo {{ scheme: \"privacy-pass\", value_raw_bytes: {} }}",
            AUTH_VALUE_RAW.len()
        ),
        "scheme と値の長さだけを表示すること"
    );
}

/// トラックを内側に持つコンテナ型の `Debug` でも機密値が伏せられること
#[test]
fn container_debug_hides_track_secrets() {
    let mut catalog = MsfCatalog::new();
    catalog.tracks.push(secret_track("video"));
    catalog.publish_tracks.push(secret_track("publish"));
    // removed_tracks は Debug に含まれる (MsfCatalog の doc) が、値は MsfTrack の Debug で伏せる
    catalog
        .removed_tracks
        .insert((None, "removed".to_string()), secret_track("removed"));

    let debug = format!("{catalog:?}");
    assert_track_redacted(&debug, "MsfCatalog");
    assert!(
        debug.contains("removed_tracks:") && debug.contains("name: \"removed\""),
        "MsfCatalog の Debug は削除済みトラックを含むこと: {debug}"
    );

    let delta = MsfDeltaUpdate {
        generated_at: None,
        operations: vec![
            MsfDeltaOperation::Add {
                tracks: vec![secret_track("added")],
            },
            MsfDeltaOperation::Clone {
                tracks: vec![secret_clone_track()],
            },
        ],
    };
    assert_track_redacted(&format!("{delta:?}"), "MsfDeltaUpdate");
    assert_track_redacted(
        &format!("{:?}", MsfCatalogDocument::Delta(delta)),
        "MsfCatalogDocument::Delta",
    );
    assert_track_redacted(
        &format!("{:?}", MsfCatalogDocument::Full(catalog)),
        "MsfCatalogDocument::Full",
    );
}

/// `MsfUri` の `Debug` が query と fragment パラメータの値を伏せること
#[test]
fn uri_debug_hides_query_and_fragment_parameter_values() {
    let uri = parse_msf_uri(
        "moqt://relay.example.com/live?auth=dummy-query-token#msf:example--video&c4m=dummy-c4m-token&token=dummy-fragment-token",
    )
    .expect("テストフィクスチャの前提条件を満たす");
    let debug = format!("{uri:?}");
    for secret in [
        "dummy-query-token",
        "dummy-c4m-token",
        "dummy-fragment-token",
    ] {
        assert_hidden(&debug, secret, "MsfUri");
    }
    assert_hidden(
        &format!("{:?}", uri.fragment),
        "dummy-c4m-token",
        "MsfFragment",
    );

    // 伏せた値は長さを表示し、名前と authority / path は表示する
    assert!(
        debug.contains(&format!("query: Some({})", "auth=dummy-query-token".len())),
        "query の長さを表示すること: {debug}"
    );
    assert!(
        debug.contains(&format!(
            "MsfFragmentParameter {{ name: \"c4m\", value_bytes: {} }}",
            "dummy-c4m-token".len()
        )),
        "c4m パラメータは名前と値の長さを表示すること: {debug}"
    );
    assert!(
        debug.contains(&format!(
            "MsfFragmentParameter {{ name: \"token\", value_bytes: {} }}",
            "dummy-fragment-token".len()
        )),
        "任意名のパラメータも値を伏せること: {debug}"
    );
    assert!(
        debug.contains("authority: \"relay.example.com\"") && debug.contains("path: \"/live\""),
        "authority と path は表示すること: {debug}"
    );
}

/// query の無い `MsfUri` の `Debug` は `query: None` を表示すること
#[test]
fn uri_debug_shows_none_for_absent_query() {
    let uri = parse_msf_uri("moqt://relay.example.com/live#msf:example--video")
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        format!("{uri:?}").contains("query: None"),
        "query が無ければ None を表示すること"
    );
}
