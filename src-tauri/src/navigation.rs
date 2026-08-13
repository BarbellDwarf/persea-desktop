//! Navigation lockdown: the webview may only navigate to the configured
//! instance origins, the OIDC/SAML identity provider hosts, and the app's
//! own local shell origin. Everything else is blocked: http(s) URLs are
//! handed to the system browser via tauri-plugin-opener, other schemes are
//! dropped. Blocked navigations log the host only, never the full URL.

use std::collections::HashSet;

use tauri::webview::{NewWindowFeatures, NewWindowResponse};
use tauri::{Runtime, Url, WebviewWindowBuilder};

/// What the webview is allowed to do with a navigation attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Let the webview navigate.
    Allow,
    /// Block the navigation and open the URL in the system browser.
    OpenExternally,
    /// Block the navigation without a system-browser hand-off.
    Deny,
}

impl Decision {
    /// Whether the navigation may proceed inside the webview.
    pub fn is_allowed(&self) -> bool {
        matches!(self, Decision::Allow)
    }
}

/// Allowlist policy for webview navigations and new-window requests.
///
/// `instance_origins` are full URLs (`scheme://host[:port]`); a bare host
/// is treated as `https://`. `extra_hosts` are bare hostnames, matched
/// case-insensitively on any scheme and port.
#[derive(Debug, Clone)]
pub struct NavigationPolicy {
    instance_origins: HashSet<url::Origin>,
    extra_hosts: HashSet<String>,
}

impl NavigationPolicy {
    /// Build the policy. Entries in `instance_origins` that are not
    /// parseable http(s) URLs are skipped with a startup warning.
    pub fn new(instance_origins: Vec<String>, extra_hosts: Vec<String>) -> Self {
        let instance_origins = instance_origins
            .into_iter()
            .filter_map(|raw| match parse_origin(&raw) {
                Some(origin) => Some(origin),
                None => {
                    log_warn(&format!(
                        "navigation lockdown: ignoring invalid instance origin {raw:?}"
                    ));
                    None
                }
            })
            .collect();
        let extra_hosts = extra_hosts
            .into_iter()
            .map(|host| host.trim().to_ascii_lowercase())
            .filter(|host| !host.is_empty())
            .collect();
        Self {
            instance_origins,
            extra_hosts,
        }
    }

    /// Navigation policy: instance origins, IdP hosts and the local shell
    /// origin are allowed; everything else is blocked, with http(s) URLs
    /// flagged for the system browser.
    pub fn classify(&self, url: &Url) -> Decision {
        if is_local_shell_origin(url) || is_dev_localhost(url) {
            return Decision::Allow;
        }
        let origin = url.origin();
        if self.instance_origins.contains(&origin) {
            return Decision::Allow;
        }
        if let Some(host) = url.host_str() {
            if self.extra_hosts.contains(&host.to_ascii_lowercase()) {
                return Decision::Allow;
            }
        }
        match url.scheme() {
            "http" | "https" => Decision::OpenExternally,
            _ => Decision::Deny,
        }
    }

    /// New-window policy (`window.open`): instance origins only, IdP
    /// hosts and the local shell are not new-window targets. D05 replaces
    /// the `Allow` arm with real window management.
    pub fn classify_new_window(&self, url: &Url) -> Decision {
        let origin = url.origin();
        if self.instance_origins.contains(&origin) {
            return Decision::Allow;
        }
        match url.scheme() {
            "http" | "https" => Decision::OpenExternally,
            _ => Decision::Deny,
        }
    }

    /// Convenience check for the navigation decision.
    pub fn allows(&self, url: &Url) -> bool {
        self.classify(url).is_allowed()
    }
}

/// Closure for [`WebviewWindowBuilder::on_navigation`]: returns `true`
/// when the navigation is allowed, otherwise blocks it and hands http(s)
/// URLs to the system browser.
pub fn navigation_handler(policy: NavigationPolicy) -> impl Fn(&Url) -> bool + Send + 'static {
    move |url: &Url| match policy.classify(url) {
        Decision::Allow => true,
        Decision::OpenExternally => {
            log_blocked(url);
            let _ = tauri_plugin_opener::open_url(url.as_str(), None::<&str>);
            false
        }
        Decision::Deny => {
            log_blocked(url);
            false
        }
    }
}

/// Closure for [`WebviewWindowBuilder::on_new_window`]: allows
/// `window.open` only for URLs inside an instance origin (default window
/// creation until D05 maps them to managed windows), rejects everything
/// else and hands http(s) URLs to the system browser.
pub fn new_window_handler<R: Runtime>(
    policy: NavigationPolicy,
) -> impl Fn(Url, NewWindowFeatures) -> NewWindowResponse<R> + Send + 'static {
    move |url: Url, _features: NewWindowFeatures| match policy.classify_new_window(&url) {
        Decision::Allow => NewWindowResponse::Allow,
        Decision::OpenExternally => {
            log_blocked(&url);
            let _ = tauri_plugin_opener::open_url(url.as_str(), None::<&str>);
            NewWindowResponse::Deny
        }
        Decision::Deny => {
            log_blocked(&url);
            NewWindowResponse::Deny
        }
    }
}

/// Wires both handlers onto a window builder. The handlers take ownership
/// of the policy, so clone it when sharing one policy across windows.
pub fn lock_window_builder<R: Runtime>(
    builder: WebviewWindowBuilder<R>,
    policy: NavigationPolicy,
) -> WebviewWindowBuilder<R> {
    builder
        .on_navigation(navigation_handler(policy.clone()))
        .on_new_window(new_window_handler(policy))
}

fn parse_origin(raw: &str) -> Option<url::Origin> {
    let raw = raw.trim();
    let candidate = if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let url = Url::parse(&candidate).ok()?;
    match url.origin() {
        origin @ url::Origin::Tuple(..) => Some(origin),
        url::Origin::Opaque(_) => None,
    }
}

/// The bundled shell origin: `tauri://localhost` on Linux/macOS,
/// `http(s)://tauri.localhost` on Windows.
fn is_local_shell_origin(url: &Url) -> bool {
    match url.host_str() {
        Some("localhost") => url.scheme() == "tauri",
        Some("tauri.localhost") => matches!(url.scheme(), "http" | "https"),
        _ => false,
    }
}

/// The tauri dev server and dev tooling live on http(s) localhost.
fn is_dev_localhost(url: &Url) -> bool {
    cfg!(dev) && matches!(url.scheme(), "http" | "https") && url.host_str() == Some("localhost")
}

/// Single logging sink for lockdown events. `tracing` is not yet a direct
/// dependency of the app crate, so events go to stderr with a stable
/// prefix; swap for `tracing::warn!` when tracing lands.
fn log_warn(message: &str) {
    eprintln!("[persea-desktop] {message}");
}

/// Log a blocked navigation with the host only, never the path or query.
fn log_blocked(url: &Url) {
    match url.host_str() {
        Some(host) => log_warn(&format!("navigation lockdown: blocked host {host}")),
        None => log_warn(&format!(
            "navigation lockdown: blocked {} URL (no host)",
            url.scheme()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn policy(instances: &[&str], hosts: &[&str]) -> NavigationPolicy {
        NavigationPolicy::new(
            instances.iter().map(|s| s.to_string()).collect(),
            hosts.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[test]
    fn instance_origin_allows_paths_and_queries() {
        let p = policy(&["https://persea.example.com"], &[]);
        assert!(p.allows(&url("https://persea.example.com/")));
        assert!(p.allows(&url("https://persea.example.com/#/sessions?x=1")));
        assert!(p.allows(&url("https://persea.example.com/redirect?token=secret")));
    }

    #[test]
    fn instance_origin_requires_matching_port() {
        let p = policy(&["https://persea.example.com:8443"], &[]);
        assert!(p.allows(&url("https://persea.example.com:8443/")));
        assert_eq!(
            p.classify(&url("https://persea.example.com:443/")),
            Decision::OpenExternally
        );
        assert_eq!(
            p.classify(&url("https://persea.example.com/")),
            Decision::OpenExternally
        );
    }

    #[test]
    fn default_ports_are_normalized() {
        let p = policy(&["https://persea.example.com:443"], &[]);
        assert!(p.allows(&url("https://persea.example.com/")));
    }

    #[test]
    fn scheme_is_part_of_the_origin() {
        let p = policy(&["https://persea.example.com"], &[]);
        assert_eq!(
            p.classify(&url("http://persea.example.com/")),
            Decision::OpenExternally
        );
    }

    #[test]
    fn subdomains_are_not_instance_origins() {
        let p = policy(&["https://persea.example.com"], &[]);
        assert_eq!(
            p.classify(&url("https://admin.persea.example.com/")),
            Decision::OpenExternally
        );
    }

    #[test]
    fn bare_host_config_defaults_to_https() {
        let p = policy(&["persea.example.com"], &[]);
        assert!(p.allows(&url("https://persea.example.com/")));
    }

    #[test]
    fn bare_host_with_port_config() {
        let p = policy(&["persea.example.com:8443"], &[]);
        assert!(p.allows(&url("https://persea.example.com:8443/")));
    }

    #[test]
    fn invalid_config_origins_are_skipped() {
        let p = policy(&[":::not a url:::", "ftp://persea.example.com"], &[]);
        assert_eq!(
            p.classify(&url("https://persea.example.com/")),
            Decision::OpenExternally
        );
        assert_eq!(
            p.classify(&url("https://elsewhere.example.com/")),
            Decision::OpenExternally
        );
    }

    #[test]
    fn ipv6_instance_origin() {
        let p = policy(&["https://[::1]:8080"], &[]);
        assert!(p.allows(&url("https://[::1]:8080/app")));
        assert_eq!(
            p.classify(&url("https://[::1]:8081/app")),
            Decision::OpenExternally
        );
    }

    #[test]
    fn extra_host_allows_idp_redirect_chain() {
        let p = policy(&["https://persea.example.com"], &["idp.example.com"]);
        assert!(p.allows(&url(
            "https://idp.example.com/authorize?client_id=abc&redirect_uri=https%3A%2F%2Fpersea.example.com%2Fcallback"
        )));
        assert!(p.allows(&url("https://idp.example.com/login")));
        assert!(p.allows(&url("http://idp.example.com:8443/login")));
    }

    #[test]
    fn extra_host_matches_any_scheme_and_port() {
        let p = policy(&[], &["idp.example.com"]);
        assert!(p.allows(&url("http://idp.example.com:8080/")));
        assert!(p.allows(&url("https://idp.example.com/")));
    }

    #[test]
    fn extra_host_is_case_insensitive() {
        let p = policy(&[], &["IDP.Example.COM"]);
        assert!(p.allows(&url("https://idp.example.com/")));
    }

    #[test]
    fn extra_host_does_not_match_subdomains() {
        let p = policy(&[], &["idp.example.com"]);
        assert_eq!(
            p.classify(&url("https://login.idp.example.com/")),
            Decision::OpenExternally
        );
    }

    #[test]
    fn local_shell_origin_is_always_allowed() {
        let p = policy(&[], &[]);
        assert!(p.allows(&url("tauri://localhost/")));
        assert!(p.allows(&url("http://tauri.localhost/")));
        assert!(p.allows(&url("https://tauri.localhost/")));
    }

    #[test]
    fn dev_server_localhost() {
        let p = policy(&[], &[]);
        let expected = if cfg!(dev) {
            Decision::Allow
        } else {
            Decision::OpenExternally
        };
        assert_eq!(p.classify(&url("http://localhost:1420/")), expected);
    }

    #[test]
    fn external_http_is_flagged_for_the_system_browser() {
        let p = policy(&["https://persea.example.com"], &[]);
        assert_eq!(
            p.classify(&url("https://docs.example.com/page?q=1")),
            Decision::OpenExternally
        );
    }

    #[test]
    fn non_http_schemes_are_dropped() {
        let p = policy(&[], &[]);
        assert_eq!(p.classify(&url("about:blank")), Decision::Deny);
        assert_eq!(p.classify(&url("javascript:void(0)")), Decision::Deny);
        assert_eq!(p.classify(&url("data:text/html,hello")), Decision::Deny);
        assert_eq!(p.classify(&url("mailto:admin@example.com")), Decision::Deny);
    }

    #[test]
    fn new_window_allows_only_instance_origins() {
        let p = policy(&["https://persea.example.com"], &["idp.example.com"]);
        assert_eq!(
            p.classify_new_window(&url("https://persea.example.com/join")),
            Decision::Allow
        );
        assert_eq!(
            p.classify_new_window(&url("https://idp.example.com/")),
            Decision::OpenExternally
        );
        assert_eq!(
            p.classify_new_window(&url("https://docs.example.com/")),
            Decision::OpenExternally
        );
        assert_eq!(
            p.classify_new_window(&url("tauri://localhost/")),
            Decision::Deny
        );
        assert_eq!(
            p.classify_new_window(&url("javascript:void(0)")),
            Decision::Deny
        );
    }

    #[test]
    fn empty_policy_fails_closed_for_web() {
        let p = policy(&[], &[]);
        assert_eq!(
            p.classify(&url("https://anything.example.com/")),
            Decision::OpenExternally
        );
        assert!(p.allows(&url("tauri://localhost/")));
    }
}
