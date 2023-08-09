use std::{collections::HashMap, sync::Arc};

use axum::{
    extract::{Path, State},
    headers::{authorization::Bearer, Authorization},
    http::StatusCode,
    TypedHeader,
};

#[tokio::main]
async fn main() {
    let cfg_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/etc/conductor/config.toml".to_string());
    let config_str =
        std::fs::read_to_string(cfg_path).expect("Expected config to exist and be valid utf-8");
    let config: Config = toml::from_str(&config_str).expect("Invalid config toml");
    let port = config.port;
    let router = axum::Router::new()
        .route("/:path", axum::routing::any(restart))
        .with_state(Arc::new(config));
    axum::Server::bind(&([0, 0, 0, 0], port).into())
        .serve(router.into_make_service())
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.unwrap();
        })
        .await
        .unwrap();
}

async fn restart(
    Path(name): Path<String>,
    State(state): State<Arc<Config>>,
    TypedHeader(Authorization(auth)): TypedHeader<Authorization<Bearer>>,
) -> Result<(StatusCode, &'static str), Error> {
    if state.token != auth.token() {
        return Err(Error::Unauthorized);
    }
    let Some(composition) = state.extra.get(&name) else {
        return Err(Error::NoComposition(name))
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

#[derive(serde::Deserialize)]
pub struct Config {
    #[serde(default = "default_port")]
    port: u16,
    token: String,
    #[serde(flatten)]
    extra: HashMap<String, ManagedComposition>,
}

fn default_port() -> u16 {
    8080
}

#[derive(serde::Deserialize)]
pub struct ManagedComposition {
    work: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error\n")]
    Io(#[from] std::io::Error),
    #[error("Docker pull failed\n")]
    PullFailed { stdout: String, stderr: String },
    #[error("Unauthorized user attempted to access server\n")]
    Unauthorized,
    #[error("No composition found for path `{0}`\n")]
    NoComposition(String),
}

impl axum::response::IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        eprintln!("{self:?}");
        let status = match self {
            Error::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Error::PullFailed {
                stdout: _,
                stderr: _,
            } => StatusCode::INTERNAL_SERVER_ERROR,
            Error::Unauthorized => StatusCode::UNAUTHORIZED,
            Error::NoComposition(_) => StatusCode::NOT_FOUND,
        };
        (status, self.to_string()).into_response()
    }
}
