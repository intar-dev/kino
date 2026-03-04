use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Clone)]
pub(crate) struct AppConfig {
    pub(crate) server_bind: ServerBind,
    pub(crate) probes: Vec<ProbeConfig>,
}

#[derive(Debug, Clone)]
pub(crate) enum ServerBind {
    Tcp(SocketAddr),
    Unix(PathBuf),
}

#[derive(Debug, Clone)]
pub(crate) struct ProbeConfig {
    pub(crate) id: String,
    pub(crate) every: Duration,
    pub(crate) timeout: Duration,
    pub(crate) kind: ProbeKindConfig,
}

#[derive(Debug, Clone)]
pub(crate) enum ProbeKindConfig {
    FileExists {
        path: PathBuf,
    },
    FileRegexCapture {
        path: PathBuf,
        pattern: String,
    },
    PortOpen {
        host: String,
        port: u16,
        protocol: PortProtocol,
    },
    K8sPodState {
        namespace: String,
        selector: String,
        desired_state: DesiredPodState,
        kubeconfig: PathBuf,
        kube_context: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PortProtocol {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DesiredPodState {
    Phase(PodPhase),
    Condition(PodCondition),
}

impl DesiredPodState {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Phase(phase) => phase.as_str(),
            Self::Condition(condition) => condition.as_str(),
        }
    }
}

impl FromStr for DesiredPodState {
    type Err = DesiredPodStateParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some((kind, raw)) = value.split_once(':') else {
            return Err(DesiredPodStateParseError::MissingPrefix {
                value: value.to_owned(),
            });
        };

        match kind {
            "phase" => PodPhase::from_str(raw).map(Self::Phase),
            "condition" => PodCondition::from_str(raw).map(Self::Condition),
            _ => Err(DesiredPodStateParseError::UnknownPrefix {
                prefix: kind.to_owned(),
                value: value.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PodPhase {
    Pending,
    Running,
    Succeeded,
    Failed,
    Unknown,
}

impl PodPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "phase:Pending",
            Self::Running => "phase:Running",
            Self::Succeeded => "phase:Succeeded",
            Self::Failed => "phase:Failed",
            Self::Unknown => "phase:Unknown",
        }
    }
}

impl FromStr for PodPhase {
    type Err = DesiredPodStateParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "Pending" => Ok(Self::Pending),
            "Running" => Ok(Self::Running),
            "Succeeded" => Ok(Self::Succeeded),
            "Failed" => Ok(Self::Failed),
            "Unknown" => Ok(Self::Unknown),
            _ => Err(DesiredPodStateParseError::UnknownPhase {
                value: value.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PodCondition {
    Ready,
    ContainersReady,
    Initialized,
    PodScheduled,
}

impl PodCondition {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "condition:Ready",
            Self::ContainersReady => "condition:ContainersReady",
            Self::Initialized => "condition:Initialized",
            Self::PodScheduled => "condition:PodScheduled",
        }
    }
}

impl FromStr for PodCondition {
    type Err = DesiredPodStateParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "Ready" => Ok(Self::Ready),
            "ContainersReady" => Ok(Self::ContainersReady),
            "Initialized" => Ok(Self::Initialized),
            "PodScheduled" => Ok(Self::PodScheduled),
            _ => Err(DesiredPodStateParseError::UnknownCondition {
                value: value.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum ConfigError {
    #[error("failed to read config file '{path}': {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse HCL config: {source}")]
    ParseHcl { source: hcl::Error },
    #[error("invalid config: {message}")]
    Validation { message: String },
}

#[derive(Debug, Error)]
pub(crate) enum DesiredPodStateParseError {
    #[error("desired_state '{value}' must use '<phase|condition>:<value>'")]
    MissingPrefix { value: String },
    #[error("desired_state '{value}' uses unknown prefix '{prefix}'")]
    UnknownPrefix { prefix: String, value: String },
    #[error("unknown phase value '{value}'")]
    UnknownPhase { value: String },
    #[error("unknown condition value '{value}'")]
    UnknownCondition { value: String },
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    server: RawServer,
    defaults: Option<RawDefaults>,
    #[serde(default)]
    probe: BTreeMap<String, RawProbe>,
}

#[derive(Debug, Deserialize)]
struct RawServer {
    bind: Option<String>,
    port: Option<u16>,
    unix_socket: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Default)]
struct RawDefaults {
    every_seconds: Option<u64>,
    timeout_seconds: Option<u64>,
    kubeconfig: Option<PathBuf>,
    kube_context: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RawProbe {
    FileExists {
        path: PathBuf,
        every_seconds: Option<u64>,
        timeout_seconds: Option<u64>,
    },
    FileRegexCapture {
        path: PathBuf,
        pattern: String,
        every_seconds: Option<u64>,
        timeout_seconds: Option<u64>,
    },
    PortOpen {
        host: String,
        port: u16,
        protocol: PortProtocol,
        every_seconds: Option<u64>,
        timeout_seconds: Option<u64>,
    },
    K8sPodState {
        namespace: String,
        selector: String,
        desired_state: String,
        kubeconfig: Option<PathBuf>,
        kube_context: Option<String>,
        every_seconds: Option<u64>,
        timeout_seconds: Option<u64>,
    },
}

#[derive(Debug, Clone)]
struct EffectiveDefaults {
    every_seconds: NonZeroU64,
    timeout_seconds: NonZeroU64,
    kubeconfig: Option<PathBuf>,
    kube_context: Option<String>,
}

pub(crate) fn load_from_file(path: &Path) -> Result<AppConfig, ConfigError> {
    let content = fs::read_to_string(path).map_err(|source| ConfigError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;

    let raw: RawConfig =
        hcl::from_str(&content).map_err(|source| ConfigError::ParseHcl { source })?;

    let defaults = normalize_defaults(raw.defaults)?;
    let server_bind = resolve_server_bind(raw.server)?;

    let probes = raw
        .probe
        .into_iter()
        .map(|(id, raw_probe)| build_probe_config(id, raw_probe, &defaults))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(AppConfig {
        server_bind,
        probes,
    })
}

fn resolve_server_bind(raw_server: RawServer) -> Result<ServerBind, ConfigError> {
    let has_tcp = raw_server.bind.is_some() || raw_server.port.is_some();
    let has_unix = raw_server.unix_socket.is_some();

    if has_tcp && has_unix {
        return Err(ConfigError::Validation {
            message: "server must define either (bind + port) or unix_socket, not both".to_owned(),
        });
    }

    if has_unix && let Some(path) = raw_server.unix_socket {
        return Ok(ServerBind::Unix(path));
    }

    if !has_tcp {
        return Err(ConfigError::Validation {
            message: "server must define either (bind + port) or unix_socket".to_owned(),
        });
    }

    let bind = raw_server.bind.ok_or_else(|| ConfigError::Validation {
        message: "server.bind is required when unix_socket is not set".to_owned(),
    })?;
    let port = raw_server.port.ok_or_else(|| ConfigError::Validation {
        message: "server.port is required when unix_socket is not set".to_owned(),
    })?;

    let bind_ip = bind
        .parse::<IpAddr>()
        .map_err(|error| ConfigError::Validation {
            message: format!("server.bind '{bind}' is not a valid IP address: {error}"),
        })?;

    Ok(ServerBind::Tcp(SocketAddr::new(bind_ip, port)))
}

fn normalize_defaults(raw_defaults: Option<RawDefaults>) -> Result<EffectiveDefaults, ConfigError> {
    let defaults = raw_defaults.unwrap_or_default();

    let every_seconds = non_zero_or_default(defaults.every_seconds, 5, "defaults.every_seconds")?;
    let timeout_seconds =
        non_zero_or_default(defaults.timeout_seconds, 2, "defaults.timeout_seconds")?;

    Ok(EffectiveDefaults {
        every_seconds,
        timeout_seconds,
        kubeconfig: defaults.kubeconfig,
        kube_context: defaults.kube_context,
    })
}

fn non_zero_or_default(
    value: Option<u64>,
    default: u64,
    field: &str,
) -> Result<NonZeroU64, ConfigError> {
    let selected = value.unwrap_or(default);

    NonZeroU64::new(selected).ok_or_else(|| ConfigError::Validation {
        message: format!("{field} must be greater than 0"),
    })
}

fn build_probe_config(
    id: String,
    raw_probe: RawProbe,
    defaults: &EffectiveDefaults,
) -> Result<ProbeConfig, ConfigError> {
    let (every, timeout, kind) = match raw_probe {
        RawProbe::FileExists {
            path,
            every_seconds,
            timeout_seconds,
        } => {
            let every = every_or_default(every_seconds, defaults.every_seconds, &id)?;
            let timeout = timeout_or_default(timeout_seconds, defaults.timeout_seconds, &id)?;
            let kind = ProbeKindConfig::FileExists { path };
            (every, timeout, kind)
        }
        RawProbe::FileRegexCapture {
            path,
            pattern,
            every_seconds,
            timeout_seconds,
        } => {
            let every = every_or_default(every_seconds, defaults.every_seconds, &id)?;
            let timeout = timeout_or_default(timeout_seconds, defaults.timeout_seconds, &id)?;
            let kind = ProbeKindConfig::FileRegexCapture { path, pattern };
            (every, timeout, kind)
        }
        RawProbe::PortOpen {
            host,
            port,
            protocol,
            every_seconds,
            timeout_seconds,
        } => {
            let every = every_or_default(every_seconds, defaults.every_seconds, &id)?;
            let timeout = timeout_or_default(timeout_seconds, defaults.timeout_seconds, &id)?;
            let kind = ProbeKindConfig::PortOpen {
                host,
                port,
                protocol,
            };
            (every, timeout, kind)
        }
        RawProbe::K8sPodState {
            namespace,
            selector,
            desired_state,
            kubeconfig,
            kube_context,
            every_seconds,
            timeout_seconds,
        } => {
            let every = every_or_default(every_seconds, defaults.every_seconds, &id)?;
            let timeout = timeout_or_default(timeout_seconds, defaults.timeout_seconds, &id)?;

            let parsed_desired_state =
                DesiredPodState::from_str(&desired_state).map_err(|error| {
                    ConfigError::Validation {
                        message: format!("probe '{id}' has invalid desired_state: {error}"),
                    }
                })?;

            let resolved_kubeconfig = kubeconfig
                .or_else(|| defaults.kubeconfig.clone())
                .ok_or_else(|| ConfigError::Validation {
                    message: format!(
                        "probe '{id}' is kind 'k8s_pod_state' but no kubeconfig is set (probe.kubeconfig or defaults.kubeconfig)"
                    ),
                })?;

            let resolved_kube_context = kube_context.or_else(|| defaults.kube_context.clone());

            let kind = ProbeKindConfig::K8sPodState {
                namespace,
                selector,
                desired_state: parsed_desired_state,
                kubeconfig: resolved_kubeconfig,
                kube_context: resolved_kube_context,
            };

            (every, timeout, kind)
        }
    };

    Ok(ProbeConfig {
        id,
        every,
        timeout,
        kind,
    })
}

fn every_or_default(
    value: Option<u64>,
    default: NonZeroU64,
    probe_id: &str,
) -> Result<Duration, ConfigError> {
    let every = value.unwrap_or_else(|| default.get());
    let non_zero = NonZeroU64::new(every).ok_or_else(|| ConfigError::Validation {
        message: format!("probe '{probe_id}' has every_seconds = 0"),
    })?;

    Ok(Duration::from_secs(non_zero.get()))
}

fn timeout_or_default(
    value: Option<u64>,
    default: NonZeroU64,
    probe_id: &str,
) -> Result<Duration, ConfigError> {
    let timeout = value.unwrap_or_else(|| default.get());
    let non_zero = NonZeroU64::new(timeout).ok_or_else(|| ConfigError::Validation {
        message: format!("probe '{probe_id}' has timeout_seconds = 0"),
    })?;

    Ok(Duration::from_secs(non_zero.get()))
}

#[cfg(test)]
mod tests {
    use super::{DesiredPodState, ServerBind, load_from_file};
    use std::fs;
    use std::str::FromStr;

    #[test]
    fn parses_probe_blocks_from_hcl() {
        let temp = tempfile::tempdir();
        assert!(temp.is_ok());
        let dir = match temp {
            Ok(value) => value,
            Err(error) => panic!("failed to create tempdir: {error}"),
        };

        let config_path = dir.path().join("kino.hcl");
        let hcl = r#"
            server {
              bind = "127.0.0.1"
              port = 9000
            }

            defaults {
              every_seconds = 5
              timeout_seconds = 2
              kubeconfig = "/tmp/kubeconfig"
            }

            probe "hosts" {
              kind = "file_exists"
              path = "/etc/hosts"
            }

            probe "ssh" {
              kind = "port_open"
              host = "127.0.0.1"
              port = 22
              protocol = "tcp"
            }
        "#;

        let write_result = fs::write(&config_path, hcl);
        assert!(write_result.is_ok());

        let loaded = load_from_file(&config_path);
        assert!(loaded.is_ok());
        let config = match loaded {
            Ok(value) => value,
            Err(error) => panic!("failed to parse config: {error}"),
        };

        match config.server_bind {
            ServerBind::Tcp(addr) => assert_eq!(addr.port(), 9000),
            ServerBind::Unix(path) => panic!("unexpected unix socket binding: {}", path.display()),
        }
        assert_eq!(config.probes.len(), 2);
    }

    #[test]
    fn parses_unix_socket_server_binding() {
        let temp = tempfile::tempdir();
        assert!(temp.is_ok());
        let dir = match temp {
            Ok(value) => value,
            Err(error) => panic!("failed to create tempdir: {error}"),
        };

        let socket_path = dir.path().join("kino.sock");
        let config_path = dir.path().join("kino.hcl");
        let hcl = format!(
            r#"
            server {{
              unix_socket = "{}"
            }}
        "#,
            socket_path.display()
        );

        let write_result = fs::write(&config_path, hcl);
        assert!(write_result.is_ok());

        let loaded = load_from_file(&config_path);
        assert!(loaded.is_ok());
        let config = match loaded {
            Ok(value) => value,
            Err(error) => panic!("failed to parse config: {error}"),
        };

        match config.server_bind {
            ServerBind::Unix(path) => assert_eq!(path, socket_path),
            ServerBind::Tcp(addr) => panic!("unexpected tcp binding: {addr}"),
        }
    }

    #[test]
    fn rejects_mixed_tcp_and_unix_server_binding() {
        let temp = tempfile::tempdir();
        assert!(temp.is_ok());
        let dir = match temp {
            Ok(value) => value,
            Err(error) => panic!("failed to create tempdir: {error}"),
        };

        let config_path = dir.path().join("kino.hcl");
        let hcl = r#"
            server {
              bind = "127.0.0.1"
              port = 9000
              unix_socket = "/tmp/kino.sock"
            }
        "#;

        let write_result = fs::write(&config_path, hcl);
        assert!(write_result.is_ok());

        let loaded = load_from_file(&config_path);
        assert!(loaded.is_err());
    }

    #[test]
    fn rejects_server_binding_when_missing_tcp_and_unix_values() {
        let temp = tempfile::tempdir();
        assert!(temp.is_ok());
        let dir = match temp {
            Ok(value) => value,
            Err(error) => panic!("failed to create tempdir: {error}"),
        };

        let config_path = dir.path().join("kino.hcl");
        let hcl = r"
            server {}
        ";

        let write_result = fs::write(&config_path, hcl);
        assert!(write_result.is_ok());

        let loaded = load_from_file(&config_path);
        assert!(loaded.is_err());
    }

    #[test]
    fn rejects_invalid_desired_state_values() {
        let parsed = DesiredPodState::from_str("condition:NotARealState");
        assert!(parsed.is_err());
    }
}
