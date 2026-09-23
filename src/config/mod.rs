//! Configuration: the shared model plus a loader per file format.
//!
//! Both loaders fill the same [`Config`] value, so everything downstream (the
//! command generator, the runners) is format-agnostic.

pub mod ini_load;
pub mod model;
pub mod template;
pub mod toml_load;

pub use model::{AuthConfig, Config, Proxy, ProxyType, Visitor};

use std::path::Path;

/// Reads a configuration file, picking the format from its content.
pub fn load(path: &Path) -> Result<Config, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    parse(&content, path)
}

/// Parses configuration text. `path` is only used for include resolution and
/// error messages.
pub fn parse(content: &str, path: &Path) -> Result<Config, String> {
    let rendered = template::render(content)?;
    if ini_load::looks_like_ini(&rendered) {
        return ini_load::parse(&rendered, path);
    }
    toml_load::parse(&rendered, path)
}
