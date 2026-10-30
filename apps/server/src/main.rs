//! ERPCons Office online server: configuration from the environment, then the `/api/v1` router
//! and the background jobs.
mod config;
mod jobs;
mod shutdown;
mod telemetry;

use std::{error::Error, net::SocketAddr, process::ExitCode, sync::Arc};

use office_identity::JwtVerifier;
use office_mail::SmtpTransport;
use office_storage::FsStorage;
use tokio::net::TcpListener;
use tracing::{error, info};

use crate::{
    config::ServerConfig,
    jobs::{Jobs, MailJob},
};

#[tokio::main]
async fn main() -> ExitCode {
    telemetry::init();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!(error = %e, "server stopped");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let config = ServerConfig::from_env()?;
    let db = office_db::connect(&config.db).await?;
    office_db::migrate(&db).await?;
    let storage = Arc::new(FsStorage::open(&config.data_dir).await?);
    let identity = Arc::new(JwtVerifier::new(config.identity)?);
    let mail = match &config.mail {
        Some(mail) => Some(MailJob {
            db: db.clone(),
            transport: Arc::new(SmtpTransport::new(mail)?),
            public_origin: mail.public_origin.clone(),
        }),
        None => None,
    };
    info!(mail = mail.is_some(), "configured");
    let state = office_api::AppState::new(db.clone(), storage, identity, config.api);
    let jobs = Jobs::spawn(state.documents(), &config.jobs, mail);
    let app = office_api::router(state).into_make_service_with_connect_info::<SocketAddr>();
    let listener = TcpListener::bind(config.bind).await?;
    info!(address = %config.bind, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown::signal())
        .await?;
    jobs.stop();
    db.close().await;
    info!("stopped");
    Ok(())
}
