use crate::connection::TursoConnection;
use crate::error::TursoError;
use crate::TursoConnectOptions;
use rbdc::error::Error;

impl TursoConnectOptions {
    /// Establish a connection to the Turso database using these options.
    ///
    /// Validates all options before attempting the connection. This enforces
    /// startup-only activation: the configuration must be complete and valid
    /// at initialization time.
    pub async fn connect_turso(&self) -> Result<TursoConnection, Error> {
        self.validate()?;

        let db = if self.in_memory {
            log::info!("turso: connecting to in-memory database");
            turso::Builder::new_local(":memory:")
                .build()
                .await
                .map_err(|e| {
                    log::error!("turso: failed to create in-memory database: {}", e);
                    TursoError::from(e)
                })?
        } else if self.is_remote() {
            // turso 0.5+ requires the sync feature and a different builder
            // for remote databases. For now, remote is not supported in this
            // adapter — use a local file path or in-memory instead.
            return Err(TursoError::configuration(
                "remote Turso databases require the 'sync' feature; \
                 use a local file path or sqlite://:memory: instead"
            ).into());
        } else {
            log::info!("turso: connecting to local database at {}", self.url);
            turso::Builder::new_local(&self.url)
                .build()
                .await
                .map_err(|e| {
                    log::error!(
                        "turso: failed to open local database {}: {}",
                        self.url,
                        e
                    );
                    TursoError::from(e)
                })?
        };

        let conn = db.connect().map_err(|e| {
            log::error!("turso: failed to obtain connection handle: {}", e);
            TursoError::from(e)
        })?;

        log::debug!("turso: connection established successfully");
        Ok(TursoConnection {
            db,
            conn,
            json_detect: self.json_detect,
        })
    }
}
