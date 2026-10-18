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
pub mod service;
pub mod store;
#[cfg(feature = "gui")]
pub mod ui;

pub const APP_ID: &str = "io.github.FarisZR.GSeek";
pub const BUS_NAME: &str = "io.github.FarisZR.GSeek.Service";
pub const OBJECT_PATH: &str = "/io/github/FarisZR/GSeek/SearchProvider";
pub const CONTROL_PATH: &str = "/io/github/FarisZR/GSeek/Service";
pub const CONTROL_IFACE: &str = "io.github.FarisZR.GSeek.Service1";
