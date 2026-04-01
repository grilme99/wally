use std::fmt;

use serde::{Deserialize, Serialize};

use crate::package_req::PackageReq;

/// Specifies how a dependency should be resolved.
///
/// Currently only registry dependencies are supported. Future variants (e.g.
/// path dependencies for workspace support) will be added here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DependencySpec {
    Registry(PackageReq),
}

impl DependencySpec {
    /// Returns the inner `PackageReq` if this is a registry dependency.
    pub fn as_registry(&self) -> Option<&PackageReq> {
        match self {
            DependencySpec::Registry(req) => Some(req),
        }
    }

    /// Unwraps the inner `PackageReq`, panicking if this is not a registry
    /// dependency. Intended for use during the transition period where only
    /// registry deps exist.
    pub fn expect_registry(&self) -> &PackageReq {
        match self {
            DependencySpec::Registry(req) => req,
        }
    }
}

impl fmt::Display for DependencySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DependencySpec::Registry(req) => write!(f, "{}", req),
        }
    }
}

impl From<PackageReq> for DependencySpec {
    fn from(req: PackageReq) -> Self {
        DependencySpec::Registry(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_round_trip_json() {
        let req: PackageReq = "hello/world@1.2.3".parse().unwrap();
        let spec = DependencySpec::Registry(req.clone());

        let json = serde_json::to_string(&spec).unwrap();
        let deserialized: DependencySpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, deserialized);

        let req_json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, req_json);
    }

    #[test]
    fn serde_round_trip_toml_value() {
        let req: PackageReq = "biff/minimal@0.1.0".parse().unwrap();
        let spec = DependencySpec::Registry(req.clone());

        let toml_val = toml::Value::try_from(&spec).unwrap();
        let deserialized: DependencySpec = toml_val.try_into().unwrap();
        assert_eq!(spec, deserialized);
    }

    #[test]
    fn deserialize_from_string() {
        let spec: DependencySpec = serde_json::from_str("\"roblox/roact@1.4.2\"").unwrap();
        assert!(spec.as_registry().is_some());
        assert_eq!(spec.expect_registry().name().scope(), "roblox");
        assert_eq!(spec.expect_registry().name().name(), "roact");
    }

    #[test]
    fn display() {
        let req: PackageReq = "hello/world@1.0.0".parse().unwrap();
        let spec = DependencySpec::Registry(req.clone());
        assert_eq!(spec.to_string(), req.to_string());
    }

    #[test]
    fn from_package_req() {
        let req: PackageReq = "foo/bar@2.0.0".parse().unwrap();
        let spec: DependencySpec = req.clone().into();
        assert_eq!(spec.expect_registry(), &req);
    }

    #[test]
    fn manifest_round_trip_toml() {
        use crate::manifest::Manifest;

        let toml_str = r#"
            [package]
            name = "test/my-package"
            version = "1.0.0"
            registry = "https://github.com/UpliftGames/wally-index"
            realm = "shared"

            [dependencies]
            Roact = "roblox/roact@1.4.0"
            Promise = "evaera/promise@3.0.0"

            [server-dependencies]
            DataStore = "sleitnick/datastore2@1.0.0"
        "#;

        let manifest: Manifest = toml::from_str(toml_str).unwrap();

        assert_eq!(manifest.dependencies.len(), 2);
        assert_eq!(manifest.server_dependencies.len(), 1);
        assert_eq!(manifest.dev_dependencies.len(), 0);

        let roact = manifest.dependencies.get("Roact").unwrap();
        assert_eq!(roact.expect_registry().name().scope(), "roblox");
        assert_eq!(roact.expect_registry().name().name(), "roact");

        let datastore = manifest.server_dependencies.get("DataStore").unwrap();
        assert_eq!(datastore.expect_registry().name().scope(), "sleitnick");

        // Round-trip: serialize back to TOML and re-parse
        let serialized = toml::to_string_pretty(&manifest).unwrap();
        let reparsed: Manifest = toml::from_str(&serialized).unwrap();
        assert_eq!(manifest.dependencies.len(), reparsed.dependencies.len());
        assert_eq!(
            manifest.server_dependencies.len(),
            reparsed.server_dependencies.len()
        );
    }
}
