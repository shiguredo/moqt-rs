//! moq-sub の CLI のふるまいを実行ファイル経由で固定する
//!
//! `--help` / `-h` がヘルプを表示して正常終了することと、`--url` / `--namespace` を
//! 省略した実行がエラーになることは、引数の解釈結果ではなくプロセスの終了コードと
//! 出力で確認する必要がある (実際の利用者の見え方と同じ経路で確かめる)。

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
            stdout.contains("--namespace"),
            "--namespace の説明が表示されること ({flag}): {stdout}"
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

/// `--namespace` を省略した実行は必須オプション欠如のエラーになる
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
