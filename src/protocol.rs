//! Protocols: creating them and loading them.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::error::Result;
use crate::paths::{read_json, write_json};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdditionalCommand {
    pub execution_command: String,
    pub language: String,
    pub ignore_laws: bool,
    pub is_assigned_worker: bool,
    pub worker_id: String,
}

/// The base schema every protocol follows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaseProtocol {
    pub name: String,
    pub description: String,
    pub path: String,
    pub commands: Vec<AdditionalCommand>,
    pub additionals: Map<String, Value>,
}

impl BaseProtocol {
    /// A placeholder protocol, useful as a template.
    pub fn sample() -> Self {
        Self {
            name: "test".into(),
            description: "sample protocol".into(),
            path: "path/to/protocol".into(),
            commands: vec![AdditionalCommand {
                execution_command: "something".into(),
                language: "python".into(),
                ignore_laws: true,
                is_assigned_worker: false,
                worker_id: "N/A".into(),
            }],
            additionals: json!({"something": "something"}).as_object().cloned().unwrap_or_default(),
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        Ok(serde_json::from_value(read_json(path)?)?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        write_json(path, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("protocol.json");
        let protocol = BaseProtocol::sample();
        protocol.save(&path).unwrap();
        assert_eq!(BaseProtocol::load(&path).unwrap(), protocol);
    }

    #[test]
    fn rejects_wrong_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        write_json(&path, &json!({"name": "x"})).unwrap();
        assert!(BaseProtocol::load(&path).is_err());
    }
}
