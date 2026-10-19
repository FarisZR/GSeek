mod cancellation;
pub mod config;
pub mod discovery;
pub mod extract;
pub mod inference;
pub mod integration;
pub mod lifecycle;
mod local_http;
mod managed_runtime;
pub mod model;
pub mod portals;
pub mod service;
pub mod store;
#[cfg(feature = "gui")]
pub mod ui;

pub const APP_ID: &str = "com.fariszr.gseek";
pub const BUS_NAME: &str = "com.fariszr.gseek.Service";
pub const OBJECT_PATH: &str = "/com/fariszr/gseek/SearchProvider";
pub const CONTROL_PATH: &str = "/com/fariszr/gseek/Service";
pub const CONTROL_IFACE: &str = "com.fariszr.gseek.Service1";
