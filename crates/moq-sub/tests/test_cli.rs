//! moq-sub の CLI のふるまいを実行ファイル経由で固定する
//!
//! `--help` / `-h` がヘルプを表示して正常終了すること、`--url` を省略した実行が
//! エラーになること、`--namespace` を省略した実行が `--url` の `msf` fragment から
//! namespace を取れること (取れない場合はエラーになること) は、引数の解釈結果ではなく
//! プロセスの終了コードと出力で確認する必要がある (実際の利用者の見え方と同じ経路で確かめる)。

use std::process::Command;

/// 引数を渡して moq-sub を実行し、終了コードと標準出力・標準エラーを返す
fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_moq-sub"))
        .args(args)
        .output()
        .expect("moq-sub を実行できること");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// `--help` はヘルプを表示して正常終了する
#[test]
fn help_prints_help_and_exits_successfully() {
    for flag in ["--help", "-h"] {
        let (code, stdout, stderr) = run(&[flag]);
        assert_eq!(code, Some(0), "正常終了すること ({flag}): {stderr}");
        assert!(
            stdout.contains("Usage:"),
            "使い方が表示されること ({flag}): {stdout}"
        );
        assert!(
            stdout.contains("--url"),
            "--url の説明が表示されること ({flag}): {stdout}"
        );
        assert!(
            stdout.contains("--insecure"),
            "--insecure の説明が表示されること ({flag}): {stdout}"
        );
        assert!(
            stdout.contains("--namespace"),
            "--namespace の説明が表示されること ({flag}): {stdout}"
        );
        assert!(
            stdout.contains("--subscribe-tracks"),
            "--subscribe-tracks の説明が表示されること ({flag}): {stdout}"
        );
        assert!(
            stdout.contains("--rendezvous-timeout-ms"),
            "--rendezvous-timeout-ms の説明が表示されること ({flag}): {stdout}"
        );
        assert!(
            stdout.contains("MOQT subscriber client"),
            "アプリの説明が表示されること ({flag}): {stdout}"
        );
        let usage = stdout
            .lines()
            .find(|line| line.starts_with("Usage:"))
            .unwrap_or("");
        assert!(
            !usage.contains("--namespace"),
            "省略可能な --namespace が Usage に必須として表示されないこと ({flag}): {usage}"
        );
        assert!(
            stdout.contains("#msf:moq-example--video"),
            "--url の例が MSF fragment を含むこと ({flag}): {stdout}"
        );
        assert!(
            stdout.contains("moqt://"),
            "URL の形式が表示されること ({flag}): {stdout}"
        );
        assert!(
            !stderr.contains("missing '--url' option")
                && !stdout.contains("missing '--url' option"),
            "必須オプションのエラーにならないこと ({flag}): {stdout}{stderr}"
        );
        assert!(
            !stderr.contains("missing '--namespace' option")
                && !stdout.contains("missing '--namespace' option"),
            "必須オプションのエラーにならないこと ({flag}): {stdout}{stderr}"
        );
    }
}

/// `--insecure` は受理され、後続の検証まで進む
///
/// 証明書検証をスキップする指定そのものが option の解釈でエラーにならないことを、
/// その後に評価される `--no-video` / `--no-audio` のエラーで確かめる (接続は行わない)。
#[test]
fn insecure_is_accepted() {
    let (code, stdout, stderr) = run(&[
        "--url",
        "moqt://127.0.0.1:4443#msf:moq-example--video",
        "--insecure",
        "--no-video",
        "--no-audio",
    ]);
    assert_eq!(code, Some(1), "異常終了すること: {stdout}{stderr}");
    assert!(
        stderr.contains("at least one of audio or video"),
        "--insecure の解釈を通って次の検証まで進むこと: {stdout}{stderr}"
    );
}

/// `--cert` と `--insecure` の同時指定はエラーになる
///
/// CA 証明書での検証と検証のスキップは同時に成り立たないため、接続を試みる前に
/// CLI で拒否する。
#[test]
fn cert_and_insecure_cannot_be_used_together() {
    let (code, stdout, stderr) = run(&[
        "--url",
        "moqt://127.0.0.1:4443#msf:moq-example--video",
        "--cert",
        "/nonexistent/ca.pem",
        "--insecure",
    ]);
    assert_eq!(code, Some(1), "異常終了すること: {stdout}{stderr}");
    assert!(
        stderr.contains("--cert") && stderr.contains("--insecure"),
        "同時指定できないことを示すこと: {stdout}{stderr}"
    );
}

/// `--url` を省略した実行は必須オプション欠如のエラーになる
///
/// ヘルプ表示用の例が必須オプションの検証を緩めないことを固定する。
#[test]
fn missing_url_is_reported_as_an_error() {
    let (code, stdout, stderr) = run(&[]);
    assert_eq!(code, Some(1), "異常終了すること: {stdout}{stderr}");
    assert!(
        stderr.contains("--url"),
        "エラーに --url が含まれること: {stdout}{stderr}"
    );
}

/// `--namespace` も `--url` の `msf` fragment も無い実行はエラーになる
///
/// 既定値の `kaki` を持たせないことを、実際の実行ファイルのふるまいとして固定する。
#[test]
fn missing_namespace_is_reported_as_an_error() {
    let (code, stdout, stderr) = run(&["--url", "moqt://127.0.0.1:4443"]);
    assert_eq!(code, Some(1), "異常終了すること: {stdout}{stderr}");
    assert!(
        stderr.contains("--namespace"),
        "エラーに --namespace が含まれること: {stdout}{stderr}"
    );
}

/// `--namespace` を省略しても `--url` の `msf` fragment から namespace を取れる
///
/// namespace の解決より後に検証されるオプションのエラーで、解決が通ったことを確かめる
/// (接続は行わない)。
#[test]
fn msf_fragment_provides_namespace_without_the_option() {
    let (code, stdout, stderr) = run(&[
        "--url",
        "moqt://127.0.0.1:4443#msf:moq-example--video",
        "--no-video",
        "--no-audio",
    ]);
    assert_eq!(code, Some(1), "異常終了すること: {stdout}{stderr}");
    assert!(
        stderr.contains("at least one of audio or video"),
        "namespace の解決より後の検証まで進むこと: {stdout}{stderr}"
    );
    assert!(
        !stderr.contains("--namespace is required"),
        "--namespace のエラーにならないこと: {stdout}{stderr}"
    );
}

/// `--url` の解析エラーは入力 URL を標準エラーに反射しない
///
/// URL の query / fragment は認可トークンを含み得て、解析エラーは標準エラーに出る。
/// noargs の InvalidOpt はオプションの生値を表示するため、値の検証エラーは生値を
/// 含まない経路で返す必要がある。
#[test]
fn invalid_url_error_does_not_echo_the_url() {
    for (url, keyword) in [
        (
            "moqt://example.com/app?token=SECRETVALUE#badvalue",
            "invalid moqt URI fragment",
        ),
        (
            "moqt://example.com/app#msf:ns--track&cat=SECRETVALUE!",
            "invalid cat parameter",
        ),
    ] {
        let (code, stdout, stderr) = run(&["--url", url]);
        assert_eq!(code, Some(1), "異常終了すること ({url}): {stdout}{stderr}");
        assert!(
            stderr.contains(keyword),
            "拒否理由が表示されること ({url}): {stdout}{stderr}"
        );
        assert!(
            stderr.contains("--url"),
            "どのオプションのエラーかを示すこと ({url}): {stdout}{stderr}"
        );
        assert!(
            !stderr.contains("SECRETVALUE"),
            "入力 URL の値を反射しないこと ({url}): {stdout}{stderr}"
        );
    }
}
