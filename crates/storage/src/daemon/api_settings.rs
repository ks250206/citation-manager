//! #26 local API listener settings: one row, validated before it is written.
use crate::error::service_error;
use cm_core::{
    ServiceError,
    policy::api::{ApiSettings, BindAddress},
};
use rusqlite::{Connection, params};
use std::path::PathBuf;

pub struct SqliteApiSettings<'a> {
    pub(crate) connection: &'a Connection,
}
impl SqliteApiSettings<'_> {
    pub fn load(&self) -> Result<ApiSettings, ServiceError> {
        let (enabled, bind, port, dirs): (bool, String, i64, String) = self
            .connection
            .query_row(
                "SELECT enabled, bind_address, port, export_dirs_json FROM api_settings WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(service_error)?;
        // Anything unexpected falls back to the safe default (off, loopback).
        let settings = ApiSettings {
            enabled,
            bind: BindAddress::parse(&bind).unwrap_or_default(),
            port: u16::try_from(port).unwrap_or(cm_core::policy::api::DEFAULT_PORT),
            export_dirs: serde_json::from_str::<Vec<PathBuf>>(&dirs).unwrap_or_default(),
        };
        Ok(if settings.validate().is_ok() {
            settings
        } else {
            ApiSettings::default()
        })
    }
    pub fn save(&self, settings: &ApiSettings, now_ms: i64) -> Result<(), ServiceError> {
        settings
            .validate()
            .map_err(|e| ServiceError::InvalidInput(e.to_string()))?;
        let dirs = serde_json::to_string(&settings.export_dirs)
            .map_err(|e| ServiceError::InvalidInput(e.to_string()))?;
        self.connection
            .execute(
                "UPDATE api_settings SET enabled = ?1, bind_address = ?2, port = ?3,
                 export_dirs_json = ?4, updated_at_ms = ?5 WHERE id = 1",
                params![
                    settings.enabled,
                    settings.bind.as_str(),
                    settings.port,
                    dirs,
                    now_ms
                ],
            )
            .map(|_| ())
            .map_err(service_error)
    }
}
