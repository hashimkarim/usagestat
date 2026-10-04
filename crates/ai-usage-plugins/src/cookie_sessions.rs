//! Manual, instance-bound cookie sessions for providers that need host injection.
use std::collections::HashMap;
use usagestat_core::ProviderConfig;

pub(super) struct Session {
    provider: String,
    cookie: Option<String>,
    user_agent: Option<String>,
}

impl Session {
    pub fn configured(provider: &str, config: Option<&ProviderConfig>) -> Option<Self> {
        if !matches!(provider, "lithosai" | "workbuddy") {
            return None;
        }
        let scoped = config
            .and_then(|c| c.instance_id.as_deref())
            .is_some_and(|id| id != provider);
        let off = config
            .and_then(|c| c.settings.get("cookies"))
            .and_then(toml::Value::as_str)
            == Some("off");
        let cookie = if off {
            None
        } else {
            match config.and_then(|c| c.cookie_header.as_ref()) {
                Some(value) => Some(value.clone()),
                None if !scoped => {
                    std::env::var(format!("{}_COOKIE", provider.to_uppercase())).ok()
                }
                None => None,
            }
            .filter(|value| !value.trim().is_empty())
        };
        let user_agent = config
            .and_then(|c| c.settings.get("browserUserAgent"))
            .and_then(toml::Value::as_str)
            .map(str::to_string);
        Some(Self {
            provider: provider.into(),
            cookie,
            user_agent,
        })
    }

    pub fn available(&self) -> bool {
        self.cookie.is_some()
    }

    pub fn headers(
        &self,
        url: &str,
        method: &str,
        mut headers: HashMap<String, String>,
    ) -> Result<HashMap<String, String>, &'static str> {
        const SCOPE: &str = "Cookie session request is outside its provider's permitted routes";
        if url.contains('\\') || url.chars().any(char::is_whitespace) {
            return Err(SCOPE);
        }
        let parsed = reqwest::Url::parse(url).map_err(|_| SCOPE)?;
        if parsed.scheme() != "https"
            || parsed.port_or_known_default() != Some(443)
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
        {
            return Err(SCOPE);
        }
        let allowed = match self.provider.as_str() {
            "lithosai" => {
                parsed.host_str() == Some("console.lithosai.cloud")
                    && method == "GET"
                    && matches!(
                        parsed.path(),
                        "/api/me" | "/api/billing" | "/api/billing/spend"
                    )
            }
            "workbuddy" => {
                parsed.host_str() == Some("www.workbuddy.cn")
                    && method == "POST"
                    && matches!(
                        parsed.path(),
                        "/billing/meter/get-user-resource-summary"
                            | "/billing/meter/get-user-resource-paid-packages"
                            | "/billing/meter/get-user-resource-free-packages"
                    )
                    && parsed.query().is_none()
            }
            _ => false,
        };
        if !allowed {
            return Err(SCOPE);
        }
        if headers.keys().any(|key| {
            matches!(
                key.to_ascii_lowercase().as_str(),
                "cookie" | "authorization" | "x-console-csrf"
            )
        }) {
            return Err("Cookie session headers must be supplied by the host");
        }
        let raw = self
            .cookie
            .as_deref()
            .ok_or("Import a manual cookie for this provider instance")?
            .trim();
        let cookie = raw
            .strip_prefix("Cookie:")
            .or_else(|| raw.strip_prefix("cookie:"))
            .unwrap_or(raw)
            .trim();
        if cookie.is_empty()
            || cookie.len() > 16384
            || !cookie.bytes().all(|b| (32..127).contains(&b))
        {
            return Err("Configured cookie header is invalid");
        }
        let mut values = HashMap::new();
        for entry in cookie.split(';') {
            let (name, value) = entry
                .trim()
                .split_once('=')
                .ok_or("Configured cookie header is invalid")?;
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
                || values.insert(name, value).is_some()
            {
                return Err("Configured cookie names are invalid or duplicated");
            }
        }
        if self.provider == "lithosai" {
            if !values
                .get("__Host-console_session")
                .is_some_and(|v| !v.is_empty())
            {
                return Err("LithosAI requires the __Host-console_session cookie");
            }
            let csrf = values
                .get("__Host-console_csrf")
                .filter(|v| !v.is_empty())
                .ok_or("LithosAI requires the __Host-console_csrf cookie from the same session")?;
            headers.insert("X-Console-Csrf".into(), (*csrf).into());
        }
        if self.provider == "workbuddy" {
            let agent = self.user_agent.as_deref().filter(|v| !v.trim().is_empty())
                .ok_or("WorkBuddy requires browserUserAgent from the browser that supplied these cookies")?;
            if agent.len() > 1024 || !agent.bytes().all(|b| (32..127).contains(&b)) {
                return Err("Configured browser User-Agent is invalid");
            }
            headers.retain(|key, _| !key.eq_ignore_ascii_case("user-agent"));
            headers.insert("User-Agent".into(), agent.into());
        }
        headers.insert("Cookie".into(), cookie.into());
        Ok(headers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session(provider: &str, cookie: &str) -> Session {
        Session {
            provider: provider.into(),
            cookie: Some(cookie.into()),
            user_agent: Some("Fixture Browser/1".into()),
        }
    }
    const COOKIE: &str = "__Host-console_session=fixture-session; __Host-console_csrf=fixture-csrf";
    #[test]
    fn csrf_is_injected_only_on_the_exact_provider_routes() {
        let s = session("lithosai", COOKIE);
        let headers = s
            .headers(
                "https://console.lithosai.cloud/api/billing/spend?start=2026-10-01&end=2026-10-04",
                "GET",
                HashMap::new(),
            )
            .unwrap();
        assert_eq!(headers["X-Console-Csrf"], "fixture-csrf");
        assert_eq!(headers["Cookie"], COOKIE);
        for url in [
            "https://console.lithosai.cloud.evil.test/api/me",
            "http://console.lithosai.cloud/api/me",
            "https://console.lithosai.cloud:444/api/me",
            "https://user@console.lithosai.cloud/api/me",
            "https://console.lithosai.cloud/api/me#fragment",
            "https://console.lithosai.cloud/api/me/extra",
            "https://console.lithosai.cloud/api/update",
        ] {
            assert!(s.headers(url, "GET", HashMap::new()).is_err());
        }
        assert!(
            s.headers(
                "https://console.lithosai.cloud/api/me",
                "POST",
                HashMap::new()
            )
            .is_err()
        );
        for name in ["Cookie", "cookie", "Authorization", "X-CONSOLE-CSRF"] {
            assert!(
                s.headers(
                    "https://console.lithosai.cloud/api/me",
                    "GET",
                    HashMap::from([(name.into(), "foreign".into())])
                )
                .is_err()
            );
        }
    }
    #[test]
    fn malformed_credentials_fail_without_exposing_values() {
        for cookie in [
            "__Host-console_session=private",
            "__Host-console_csrf=private",
            "__Host-console_session=private; __Host-console_csrf=private; __Host-console_csrf=other",
            "__Host-console_session=private\r\nX-Test: bad; __Host-console_csrf=private",
        ] {
            let error = session("lithosai", cookie)
                .headers(
                    "https://console.lithosai.cloud/api/me",
                    "GET",
                    HashMap::new(),
                )
                .unwrap_err();
            assert!(!error.contains("private"));
        }
    }
    #[test]
    fn workbuddy_uses_the_configured_browser_identity_and_scoped_cookie() {
        let mut s = session("workbuddy", "session=fixture");
        let url = "https://www.workbuddy.cn/billing/meter/get-user-resource-summary";
        let headers = s
            .headers(
                url,
                "POST",
                HashMap::from([("user-agent".into(), "script-default".into())]),
            )
            .unwrap();
        assert_eq!(headers["User-Agent"], "Fixture Browser/1");
        assert!(!headers.contains_key("user-agent"));
        assert!(!headers.contains_key("X-Console-Csrf"));
        assert!(s.headers(url, "GET", HashMap::new()).is_err());
        s.user_agent = None;
        assert!(
            s.headers(url, "POST", HashMap::new())
                .unwrap_err()
                .contains("browserUserAgent")
        );
    }
    #[test]
    fn additional_instances_and_disabled_cookie_access_never_use_ambient_credentials() {
        let config: ProviderConfig =
            toml::from_str("id = 'lithosai'\ninstanceId = 'other-account'").unwrap();
        assert!(
            !Session::configured("lithosai", Some(&config))
                .unwrap()
                .available()
        );
        let config: ProviderConfig = toml::from_str(
            "id = 'lithosai'\ncookieHeader = 'private'\n[settings]\ncookies = 'off'",
        )
        .unwrap();
        assert!(
            !Session::configured("lithosai", Some(&config))
                .unwrap()
                .available()
        );
    }
}
