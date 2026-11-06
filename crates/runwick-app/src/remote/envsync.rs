//! Global environment variables across devices. Every host keeps its own list and version;
//! this computer can send its NON-secret values to a device on request. Secrets never leave
//! a host this way: they are entered on each device.

use runwick_core::api::env::{EnvState, EnvVar, SetVar};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct EnvDiff {
    /// Non-secret variables of this computer that the device lacks or has with another value.
    pub to_send: Vec<EnvVar>,
    /// Secret names of this computer (never sent; entered on the device if needed).
    pub local_secrets: Vec<String>,
    /// Names the device keeps as secrets while this computer has a plain value (left alone).
    pub secret_there: Vec<String>,
    /// Variables only the device has (left alone).
    pub only_there: usize,
}

impl EnvDiff {
    pub fn in_sync(&self) -> bool {
        self.to_send.is_empty()
    }
}

pub fn diff(local: &EnvState, device: &EnvState) -> EnvDiff {
    let mut d = EnvDiff::default();
    for v in &local.vars {
        if v.secret {
            d.local_secrets.push(v.key.clone());
            continue;
        }
        match device.vars.iter().find(|x| x.key == v.key) {
            Some(x) if x.secret => d.secret_there.push(v.key.clone()),
            Some(x) if x.value == v.value && x.description == v.description => {}
            _ => d.to_send.push(v.clone()),
        }
    }
    d.only_there = device.vars.iter().filter(|x| !local.vars.iter().any(|v| v.key == x.key)).count();
    d
}

/// Requests that send the differences (non-secret only, never removing anything there).
pub fn requests(d: &EnvDiff) -> Vec<SetVar> {
    d.to_send
        .iter()
        .filter(|v| !v.secret)
        .map(|v| SetVar {
            key: v.key.clone(),
            value: v.value.clone(),
            description: v.description.clone(),
            secret: false,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use runwick_core::api::env::{SecretStoreInfo, SecretStoreKind};

    fn var(key: &str, value: Option<&str>, secret: bool) -> EnvVar {
        EnvVar {
            key: key.into(),
            description: String::new(),
            secret,
            value: value.map(str::to_string),
            changed_in: 1,
            updated_at: runwick_core::now(),
        }
    }

    fn state(vars: Vec<EnvVar>, version: u64) -> EnvState {
        EnvState {
            version,
            vars,
            secrets: SecretStoreInfo { kind: SecretStoreKind::Keychain, location: "x".into() },
            saved_at: None,
        }
    }

    #[test]
    fn only_non_secret_differences_are_sent() {
        let here = state(
            vec![
                var("HTTP_PROXY", Some("http://proxy:8080"), false),
                var("LANG", Some("ko_KR.UTF-8"), false),
                var("GITHUB_TOKEN", None, true),
                var("API_URL", Some("https://a"), false),
            ],
            12,
        );
        let there = state(
            vec![
                var("HTTP_PROXY", Some("http://old:8080"), false),
                var("LANG", Some("ko_KR.UTF-8"), false),
                var("API_URL", None, true),
                var("DEVICE_ONLY", Some("1"), false),
            ],
            11,
        );
        let d = diff(&here, &there);
        assert_eq!(d.to_send.iter().map(|v| v.key.as_str()).collect::<Vec<_>>(), vec!["HTTP_PROXY"]);
        assert_eq!(d.local_secrets, vec!["GITHUB_TOKEN"]);
        assert_eq!(d.secret_there, vec!["API_URL"], "a secret on the device is never overwritten");
        assert_eq!(d.only_there, 1);
        let r = requests(&d);
        assert_eq!(r.len(), 1);
        assert!(!r[0].secret && r[0].value.as_deref() == Some("http://proxy:8080"));
        assert!(diff(&here, &here).in_sync());
    }
}
