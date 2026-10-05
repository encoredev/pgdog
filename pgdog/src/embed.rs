//! Running PgDog inside another program.
//!
//! [`serve`] runs the pooler on the caller's Tokio runtime, with a config
//! built in memory and a listener the caller has already bound, until
//! [`shutdown`] has drained it. PgDog's state is process-wide and a shutdown
//! is final, so a process serves once.
//!
//! Unlike the `pgdog` binary, it installs no logger, no allocator and no
//! signal handlers: SIGTERM, SIGINT and SIGHUP belong to the program embedding
//! it, and a config set in memory has no files to reload. It serves no admin
//! database either, so a client the config trusts cannot administer it. Its
//! logs are `tracing` events, for that program's subscriber.

use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::net::TcpListener;

pub use pgdog_config;
use pgdog_config::ConfigAndUsers;

use crate::backend::databases;
use crate::frontend::comms::comms;
use crate::frontend::listener::{Listener, Signals};
use crate::frontend::prepared_statements;
use crate::{api, config, healthcheck, net, plugin, stats, tasks};

/// Set once [`serve`] has been called: PgDog is embedded.
static EMBEDDED: AtomicBool = AtomicBool::new(false);

/// Whether PgDog runs embedded, through [`serve`].
pub(crate) fn embedded() -> bool {
    EMBEDDED.load(Ordering::Relaxed)
}

/// Serve the clients `listener` accepts with `config`, until [`shutdown`] has
/// drained them and closed the server connections.
pub async fn serve(
    config: ConfigAndUsers,
    listener: TcpListener,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    EMBEDDED.store(true, Ordering::Relaxed);
    config::set(config)?;
    plugin::load_from_config()?;
    net::tls::load()?;
    databases::init()?;

    let config = config::config();
    let general = &config.config.general;
    if let Some(openmetrics_port) = general.openmetrics_port {
        let openmetrics_host = general.openmetrics_host.clone();
        tasks::spawn("openmetrics server", async move {
            stats::http_server::server(&openmetrics_host, openmetrics_port).await
        });
    }
    if config.config.otel.endpoint.is_some() {
        tasks::spawn("otel publisher", stats::otel_exporter::run());
    }
    if let Some(healthcheck_port) = general.healthcheck_port {
        tasks::spawn("http healthcheck server", async move {
            healthcheck::server(healthcheck_port).await
        });
    }
    prepared_statements::start_maintenance();

    let mut pgdog = Listener::new(listener.local_addr()?);
    let served = pgdog.listen_on(listener, Signals::Ignore).await;

    databases::shutdown();
    api::tasks_storage().cancel_all();
    tasks::shutdown().await;
    plugin::shutdown();
    Ok(served?)
}

/// Stop taking clients and drain: clients get up to the config's
/// `shutdown_timeout` to finish their transactions, then [`serve`] returns.
pub fn shutdown() {
    comms().shutdown();
}
