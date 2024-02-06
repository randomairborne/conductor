use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    extract::{Path, State},
    http::StatusCode,
};
use axum_extra::{
    headers::{authorization::Bearer, Authorization},
    TypedHeader,
};
use tokio::{net::TcpListener, select, task::JoinSet, time::MissedTickBehavior};

#[tokio::main]
async fn main() {
    let cfg_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/etc/conductor/config.toml".to_string());
    let config_str =
        std::fs::read_to_string(cfg_path).expect("Expected config to exist and be valid utf-8");
    let config: Config = toml::from_str(&config_str).expect("Invalid config toml");
    let config = Arc::new(config);
    let mut workers = JoinSet::new();
    if let Some(secs) = config.force_update_interval {
        workers.spawn(restart_all(secs, config.clone()));
    }
    if let Some(secs) = config.prune_interval {
        workers.spawn(prune(secs));
    }
    let port = config.port;
    let app = axum::Router::new()
        .route("/:path", axum::routing::any(restart_web))
        .with_state(config);
    let bind_address = SocketAddr::from(([0, 0, 0, 0], port));
    println!("Starting server on http://localhost:8080");
    let tcp = TcpListener::bind(bind_address).await.unwrap();
    axum::serve(tcp, app)
        .with_graceful_shutdown(vss::shutdown_signal())
        .await
        .unwrap();
    while let Some(val) = workers.join_next().await {
        if let Err(err) = val {
            eprintln!("Error on shutdown: {err:?}");
        }
    }
}

async fn restart_web(
    Path(name): Path<String>,
    State(state): State<Arc<Config>>,
    TypedHeader(Authorization(auth)): TypedHeader<Authorization<Bearer>>,
) -> Result<(StatusCode, &'static str), Error> {
    if state.token != auth.token() {
        return Err(Error::Unauthorized);
    }
    if let Err(source) = restart(&name, state).await {
        eprintln!("Error: {source:?}");
        Err(source)
    } else {
        Ok((StatusCode::OK, "Success\n"))
    }
}

async fn restart_all(secs: u64, config: Arc<Config>) {
    let period = Duration::from_secs(secs);
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = ticker.tick() => {}
        }
        for name in config.extra.keys() {
            if let Err(source) = restart(name, config.clone()).await {
                eprintln!("Error: {source:?}")
            }
        }
    }
}

async fn restart(name: &str, config: Arc<Config>) -> Result<(StatusCode, &'static str), Error> {
    let Some(composition) = config.extra.get(name) else {
        return Err(Error::NoComposition(name.to_owned()));
    };
    let pull_task = tokio::process::Command::new("docker")
        .arg("compose")
        .arg("up")
        .arg("-d")
        .arg("--pull")
        .arg("always")
        .current_dir(&composition.work)
        .spawn()?;
    let output = pull_task.wait_with_output().await?;
    if !output.status.success() {
        Err(Error::PullFailed {
            stdout: String::from_utf8_lossy(&output.stdout).into(),
            stderr: String::from_utf8_lossy(&output.stderr).into(),
        })
    } else {
        Ok((StatusCode::OK, "Success\n"))
    }
}

async fn prune(secs: u64) {
    let period = Duration::from_secs(secs);
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = ticker.tick() => {}
        }
        if let Err(source) = do_prune().await {
            eprintln!("Error: {source:?}")
        }
    }
}

async fn do_prune() -> Result<(), Error> {
    let pull_task = tokio::process::Command::new("docker")
        .arg("image")
        .arg("prune")
        .arg("-a")
        .arg("-f")
        .spawn()?;
    let output = pull_task.wait_with_output().await?;
    if !output.status.success() {
        Err(Error::PruneFailed {
            stdout: String::from_utf8_lossy(&output.stdout).into(),
            stderr: String::from_utf8_lossy(&output.stderr).into(),
        })
    } else {
        Ok(())
    }
}

#[derive(serde::Deserialize)]
pub struct Config {
    #[serde(default = "default_port")]
    port: u16,
    token: String,
    force_update_interval: Option<u64>,
    prune_interval: Option<u64>,
    #[serde(flatten)]
    extra: HashMap<String, ManagedComposition>,
}

#[derive(serde::Deserialize)]
pub struct ManagedComposition {
    work: String,
}

fn default_port() -> u16 {
    8080
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error\n")]
    Io(#[from] std::io::Error),
    #[error("Docker pull failed\n")]
    PullFailed { stdout: String, stderr: String },
    #[error("Docker prune failed\n")]
    PruneFailed { stdout: String, stderr: String },
    #[error("No composition found for path `{0}`\n")]
    NoComposition(String),
    #[error("Unauthorized user attempted to access server\n")]
    Unauthorized,
}

impl axum::response::IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        eprintln!("Error: `{self:?}`");
        let status = match self {
            Error::Io(_) | Error::PullFailed { .. } | Error::PruneFailed { .. } => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Error::Unauthorized => StatusCode::UNAUTHORIZED,
            Error::NoComposition(_) => StatusCode::NOT_FOUND,
        };
        (status, self.to_string()).into_response()
    }
}
