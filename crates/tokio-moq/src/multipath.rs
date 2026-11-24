//! Multipath QUIC (RFC 10069) の設定
//!
//! tokio-moq から tobi-quic の Multipath QUIC を使うための設定値と、その検証を提供する。
//! パスの開設・検証・切り替えは QUIC 層が行うため、MOQT の Session からは透過である。

use std::time::Duration;

/// 追加パスに使える Path ID の上限
///
/// tobi-quic の `Connection::set_max_path_id` が受け付ける上限に合わせる。Path ID は
/// 接続の生存期間で使い回さないため、この値は接続中に開ける追加パスの総数の上限になる
/// (RFC 10069 Section 3)。
const MAX_PATH_ID: u32 = 64;

/// パスの使い方
///
/// tobi-quic の `MultipathMode` に対応する。`Off` 以外を指定したときだけ Multipath QUIC を
/// 広告する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultipathMode {
    /// Multipath を使わない (追加パスを開かず、ピアからの追加パスも受け付けない)
    Off,
    /// 最良の 1 本で送り、使えなくなったときやより良いパスが現れたときに切り替える
    Handover,
    /// 送信に使えるすべてのパスで同時に送る
    Aggregate,
    /// 最初のパスで送り続け、使えなくなったときだけ待機パスへ切り替える
    Backup,
}

impl MultipathMode {
    /// CLI の値を解釈する
    ///
    /// 解釈できない値は、受け付ける値の一覧を含むエラーを返す。
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "off" => Ok(Self::Off),
            "handover" => Ok(Self::Handover),
            "aggregate" => Ok(Self::Aggregate),
            "backup" => Ok(Self::Backup),
            other => Err(format!(
                "unsupported multipath mode: {other} (supported values: off, handover, aggregate, backup)"
            )),
        }
    }

    /// 設定値としての名前を返す
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Handover => "handover",
            Self::Aggregate => "aggregate",
            Self::Backup => "backup",
        }
    }

    /// Multipath QUIC を有効にするか
    pub fn is_enabled(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// 自動でパスを開くネットワークインターフェースのフィルタ
///
/// tobi-quic の `InterfaceFilter` に対応する。ネットワーク監視を有効にしたときだけ意味を
/// 持つ。`open_path` で明示的に開くパスはこのフィルタの対象外である。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterfaceFilter {
    /// すべてのインターフェースを対象にする
    All,
    /// 指定した名前のインターフェースだけを対象にする
    Allow(Vec<String>),
    /// 指定した名前のインターフェースを除外する
    Deny(Vec<String>),
}

/// Multipath QUIC の設定
///
/// [`MultipathMode::Off`] の場合は Multipath QUIC を広告しない。有効にできるのは
/// tobi-quic のネイティブ QUIC 経路だけであり、WebTransport / QMux では接続時に
/// エラーになる。Windows では tobi-quic が Multipath QUIC を実装していないため使えない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipathOptions {
    /// パスの使い方
    pub mode: MultipathMode,
    /// 自動でパスを開くインターフェースのフィルタ
    pub interface_filter: InterfaceFilter,
    /// 自動で開くパスの同時数の上限
    ///
    /// `open_path` で明示的に開くパスは制限しない。tobi-quic の `MultipathConfig::max_paths`
    /// に対応する。
    pub max_paths: usize,
    /// 自分が通知する追加パスの Path ID の上限
    ///
    /// 接続の生存期間で使い回さないため、接続中に開ける追加パスの総数の上限になる。
    /// 64 を超える値は指定できない。
    pub initial_max_path_id: u32,
    /// ネットワークインターフェースの監視間隔
    ///
    /// `Some` の場合はインターフェースの増減を監視し、変化に応じてパスを自動で開閉する。
    /// `None` の場合は監視しない。
    pub network_monitor_interval: Option<Duration>,
    /// 待機パスの keep-alive の送信間隔
    ///
    /// `Handover` と `Backup` では、待機させているパスを middlebox のタイムアウトで失わない
    /// ように keep-alive を送る (RFC 10069 Section 5.9)。`None` の場合は接続のアイドル
    /// タイムアウトから間隔を決める。アイドルタイムアウトも無効な場合は keep-alive が動かない。
    pub keep_alive_interval: Option<Duration>,
}

impl Default for MultipathOptions {
    fn default() -> Self {
        Self {
            mode: MultipathMode::Off,
            interface_filter: InterfaceFilter::All,
            max_paths: 2,
            initial_max_path_id: 4,
            network_monitor_interval: None,
            keep_alive_interval: None,
        }
    }
}

impl MultipathOptions {
    /// 設定値を検証する
    ///
    /// 範囲外の値や、空のインターフェース名を含む場合はエラーを返す。`mode` が `Off` の
    /// 場合は、Multipath 固有の設定が既定のままであることも検証する。エラーの文字列は
    /// 利用者向けの設定エラーとしてそのまま表示できる。
    pub fn validate(&self) -> Result<(), String> {
        if !self.mode.is_enabled() {
            // 既定値と比較し、Off のまま Multipath 固有の設定が残っていないかを見る。
            // 既定値を 1 箇所 (Default 実装) に保つため、ここで数値を書かない。
            let defaults = Self::default();
            if self.interface_filter != defaults.interface_filter
                || self.max_paths != defaults.max_paths
                || self.initial_max_path_id != defaults.initial_max_path_id
                || self.network_monitor_interval.is_some()
                || self.keep_alive_interval.is_some()
            {
                return Err(
                    "multipath settings require a multipath mode other than off".to_string()
                );
            }
        }
        if self.max_paths == 0 {
            return Err("multipath max_paths must be greater than zero".to_string());
        }
        if self.max_paths > MAX_PATH_ID as usize {
            return Err(format!(
                "multipath max_paths must be at most {MAX_PATH_ID}: {}",
                self.max_paths
            ));
        }
        if self.initial_max_path_id == 0 {
            return Err("multipath initial_max_path_id must be greater than zero".to_string());
        }
        if self.initial_max_path_id > MAX_PATH_ID {
            return Err(format!(
                "multipath initial_max_path_id must be at most {MAX_PATH_ID}: {}",
                self.initial_max_path_id
            ));
        }
        if let Some(interval) = self.network_monitor_interval
            && interval.is_zero()
        {
            return Err("multipath network monitor interval must be greater than zero".to_string());
        }
        if let Some(interval) = self.keep_alive_interval
            && interval.is_zero()
        {
            return Err("multipath keep-alive interval must be greater than zero".to_string());
        }
        if let InterfaceFilter::Allow(names) | InterfaceFilter::Deny(names) = &self.interface_filter
        {
            if names.is_empty() {
                return Err(
                    "multipath interface filter must contain at least one interface name"
                        .to_string(),
                );
            }
            if let Some(name) = names.iter().find(|name| name.trim().is_empty()) {
                return Err(format!(
                    "multipath interface name must not be empty: {name:?}"
                ));
            }
        }
        Ok(())
    }
}

/// カンマ区切りのインターフェース名を解釈する
///
/// 前後の空白を取り除く。空の要素を含む場合はエラーを返す。CLI の
/// `--multipath-interface` / `--multipath-interface-deny` が使う。
pub fn parse_interface_names(value: &str) -> Result<Vec<String>, String> {
    let names: Vec<String> = value
        .split(',')
        .map(|name| name.trim().to_string())
        .collect();
    if names.iter().any(String::is_empty) {
        return Err(format!(
            "multipath interface list must contain non-empty comma-separated names: {value:?}"
        ));
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// モードの文字列表現とパースが往復すること
    #[test]
    fn multipath_mode_round_trips() {
        for mode in [
            MultipathMode::Off,
            MultipathMode::Handover,
            MultipathMode::Aggregate,
            MultipathMode::Backup,
        ] {
            assert_eq!(
                MultipathMode::parse(mode.as_str()).expect("解釈できること"),
                mode,
                "文字列表現をパースすると元のモードに戻ること: {}",
                mode.as_str()
            );
        }
    }

    /// 未知のモードは受け付ける値の一覧を含めて拒否すること
    #[test]
    fn multipath_mode_rejects_unknown_values() {
        let error = MultipathMode::parse("redundant").expect_err("未知のモードは拒否すること");
        assert!(
            error.contains("off, handover, aggregate, backup"),
            "受け付ける値の一覧を含めること: {error}"
        );
    }

    /// 既定値は Multipath を無効にし、tobi-quic の既定のパス数に合わせること
    #[test]
    fn multipath_options_default_to_disabled() {
        let options = MultipathOptions::default();
        assert_eq!(options.mode, MultipathMode::Off);
        assert_eq!(options.interface_filter, InterfaceFilter::All);
        assert_eq!(options.max_paths, 2);
        assert_eq!(options.initial_max_path_id, 4);
        assert!(options.network_monitor_interval.is_none());
        assert!(options.keep_alive_interval.is_none());
        options.validate().expect("既定値は妥当であること");
    }

    /// 範囲外のパス数と Path ID を拒否すること
    #[test]
    fn multipath_options_reject_out_of_range_path_counts() {
        let options = MultipathOptions {
            mode: MultipathMode::Handover,
            max_paths: 0,
            ..MultipathOptions::default()
        };
        assert!(options.validate().is_err(), "max_paths 0 は拒否すること");

        let options = MultipathOptions {
            mode: MultipathMode::Handover,
            max_paths: MAX_PATH_ID as usize + 1,
            ..MultipathOptions::default()
        };
        assert!(
            options.validate().is_err(),
            "max_paths が上限を超える場合は拒否すること"
        );

        let options = MultipathOptions {
            mode: MultipathMode::Handover,
            initial_max_path_id: 0,
            ..MultipathOptions::default()
        };
        assert!(
            options.validate().is_err(),
            "initial_max_path_id 0 は拒否すること"
        );

        let options = MultipathOptions {
            mode: MultipathMode::Handover,
            initial_max_path_id: MAX_PATH_ID + 1,
            ..MultipathOptions::default()
        };
        assert!(
            options.validate().is_err(),
            "initial_max_path_id が上限を超える場合は拒否すること"
        );
    }

    /// 0 の監視間隔と keep-alive 間隔を拒否すること
    #[test]
    fn multipath_options_reject_zero_intervals() {
        let options = MultipathOptions {
            mode: MultipathMode::Handover,
            network_monitor_interval: Some(Duration::ZERO),
            ..MultipathOptions::default()
        };
        assert!(options.validate().is_err(), "監視間隔 0 は拒否すること");

        let options = MultipathOptions {
            mode: MultipathMode::Handover,
            keep_alive_interval: Some(Duration::ZERO),
            ..MultipathOptions::default()
        };
        assert!(
            options.validate().is_err(),
            "keep-alive 間隔 0 は拒否すること"
        );
    }

    /// 空のインターフェース名を拒否すること
    #[test]
    fn multipath_options_reject_empty_interface_names() {
        let options = MultipathOptions {
            mode: MultipathMode::Handover,
            interface_filter: InterfaceFilter::Allow(Vec::new()),
            ..MultipathOptions::default()
        };
        assert!(options.validate().is_err(), "空の許可リストは拒否すること");

        let options = MultipathOptions {
            mode: MultipathMode::Handover,
            interface_filter: InterfaceFilter::Deny(vec!["eth0".to_string(), " ".to_string()]),
            ..MultipathOptions::default()
        };
        assert!(
            options.validate().is_err(),
            "空白だけのインターフェース名は拒否すること"
        );
    }

    /// モードが Off なのに Multipath 固有の設定が残っている場合を拒否すること
    #[test]
    fn multipath_options_reject_settings_with_off_mode() {
        let options = MultipathOptions {
            keep_alive_interval: Some(Duration::from_secs(10)),
            ..MultipathOptions::default()
        };
        assert!(
            options.validate().is_err(),
            "モードが Off のときは Multipath 固有の設定を残さないこと"
        );
    }

    /// カンマ区切りのインターフェース名を前後の空白を除いて解釈すること
    #[test]
    fn parse_interface_names_trims_names() {
        assert_eq!(
            parse_interface_names("en0, wlan0").expect("解釈できること"),
            vec!["en0".to_string(), "wlan0".to_string()],
            "名前の前後の空白を取り除くこと"
        );
    }

    /// 空の要素を含むインターフェース名の一覧を拒否すること
    #[test]
    fn parse_interface_names_rejects_empty_entries() {
        for value in ["", "en0,", ",wlan0", "en0,,wlan0", "en0, ,wlan0"] {
            assert!(
                parse_interface_names(value).is_err(),
                "空の要素を含む一覧を拒否すること: {value:?}"
            );
        }
    }
}
