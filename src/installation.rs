use std::{
    fmt::Display,
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, format_err};
use crossterm::style::{Color, SetForegroundColor};
use fs_err as fs;
use indicatif::{ProgressBar, ProgressStyle};
use indoc::{formatdoc, indoc};

use crate::{
    manifest::Realm,
    package_contents::PackageContents,
    package_id::PackageId,
    package_source::{PackageSourceMap, PackageSourceProvider},
    resolution::Resolve,
};

/// Distinguishes between the two container directories within each realm's
/// package folder (`Packages/`, `ServerPackages/`, `DevPackages/`).
///
/// - `Index` (`_Index/`) holds unpacked registry packages.
/// - `Workspace` (`_Workspace/`) holds Rojo project references to local
///   workspace members.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageContainer {
    Index,
    Workspace,
}

impl PackageContainer {
    pub fn dir_name(&self) -> &'static str {
        match self {
            PackageContainer::Index => "_Index",
            PackageContainer::Workspace => "_Workspace",
        }
    }
}

/// Determine which container a resolved package belongs in based on whether
/// it is a workspace member.
fn package_container(resolved: &Resolve, package_id: &PackageId) -> PackageContainer {
    resolved
        .metadata
        .get(package_id)
        .map(|m| {
            if m.is_workspace_member {
                PackageContainer::Workspace
            } else {
                PackageContainer::Index
            }
        })
        .unwrap_or(PackageContainer::Index)
}

#[derive(Clone)]
pub struct InstallationContext {
    shared_dir: PathBuf,
    shared_index_dir: PathBuf,
    shared_workspace_dir: PathBuf,
    shared_path: Option<String>,
    server_dir: PathBuf,
    server_index_dir: PathBuf,
    server_workspace_dir: PathBuf,
    server_path: Option<String>,
    dev_dir: PathBuf,
    dev_index_dir: PathBuf,
    dev_workspace_dir: PathBuf,
}

impl InstallationContext {
    /// Create a new `InstallationContext` for the given path.
    pub fn new(
        project_path: &Path,
        shared_path: Option<String>,
        server_path: Option<String>,
    ) -> Self {
        let shared_dir = project_path.join("Packages");
        let server_dir = project_path.join("ServerPackages");
        let dev_dir = project_path.join("DevPackages");

        let shared_index_dir = shared_dir.join("_Index");
        let server_index_dir = server_dir.join("_Index");
        let dev_index_dir = dev_dir.join("_Index");

        let shared_workspace_dir = shared_dir.join("_Workspace");
        let server_workspace_dir = server_dir.join("_Workspace");
        let dev_workspace_dir = dev_dir.join("_Workspace");

        Self {
            shared_dir,
            shared_index_dir,
            shared_workspace_dir,
            shared_path,
            server_dir,
            server_index_dir,
            server_workspace_dir,
            server_path,
            dev_dir,
            dev_index_dir,
            dev_workspace_dir,
        }
    }

    /// Delete the existing package directories, if they exist.
    pub fn clean(&self) -> anyhow::Result<()> {
        fn remove_ignore_not_found(path: &Path) -> io::Result<()> {
            if let Err(err) = fs::remove_dir_all(path) {
                if err.kind() != io::ErrorKind::NotFound {
                    return Err(err);
                }
            }

            Ok(())
        }

        remove_ignore_not_found(&self.shared_dir)?;
        remove_ignore_not_found(&self.server_dir)?;
        remove_ignore_not_found(&self.dev_dir)?;

        Ok(())
    }

    /// Return the container directory for a given realm and container type.
    fn container_dir(&self, realm: Realm, container: PackageContainer) -> &Path {
        match (realm, container) {
            (Realm::Shared, PackageContainer::Index) => &self.shared_index_dir,
            (Realm::Shared, PackageContainer::Workspace) => &self.shared_workspace_dir,
            (Realm::Server, PackageContainer::Index) => &self.server_index_dir,
            (Realm::Server, PackageContainer::Workspace) => &self.server_workspace_dir,
            (Realm::Dev, PackageContainer::Index) => &self.dev_index_dir,
            (Realm::Dev, PackageContainer::Workspace) => &self.dev_workspace_dir,
        }
    }

    /// Install all packages from the given `Resolve` into the package that this
    /// `InstallationContext` was built for.
    pub fn install(
        self,
        sources: PackageSourceMap,
        root_package_id: PackageId,
        resolved: Resolve,
    ) -> anyhow::Result<()> {
        let mut handles = Vec::new();
        let resolved_copy = resolved.clone();
        let bar = ProgressBar::new((resolved_copy.activated.len() - 1) as u64).with_style(
            ProgressStyle::with_template(
                "{spinner:.cyan.bold} {pos}/{len} [{wide_bar:.cyan/blue}]",
            )
            .unwrap()
            .tick_chars("⠁⠈⠐⠠⠄⠂ ")
            .progress_chars("#>-"),
        );
        bar.enable_steady_tick(Duration::from_millis(100));

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(50)
            .enable_all()
            .build()
            .unwrap();

        for package_id in resolved_copy.activated {
            log::debug!("Installing {}...", package_id);

            let shared_deps = resolved.shared_dependencies.get(&package_id);
            let server_deps = resolved.server_dependencies.get(&package_id);
            let dev_deps = resolved.dev_dependencies.get(&package_id);

            // We do not need to install the root package, but we should create
            // package links for its dependencies.
            if package_id == root_package_id {
                if let Some(deps) = shared_deps {
                    self.write_root_package_links(Realm::Shared, deps, &resolved)?;
                }

                if let Some(deps) = server_deps {
                    self.write_root_package_links(Realm::Server, deps, &resolved)?;
                }

                if let Some(deps) = dev_deps {
                    self.write_root_package_links(Realm::Dev, deps, &resolved)?;
                }
            } else {
                let metadata = resolved.metadata.get(&package_id).unwrap();
                let package_realm = metadata.origin_realm;

                if let Some(deps) = shared_deps {
                    self.write_package_links(&package_id, package_realm, deps, &resolved)?;
                }

                if let Some(deps) = server_deps {
                    self.write_package_links(&package_id, package_realm, deps, &resolved)?;
                }

                if let Some(deps) = dev_deps {
                    self.write_package_links(&package_id, package_realm, deps, &resolved)?;
                }

                let source_registry = resolved_copy.metadata[&package_id].source_registry.clone();
                let container = if resolved_copy.metadata[&package_id].is_workspace_member {
                    PackageContainer::Workspace
                } else {
                    PackageContainer::Index
                };
                let source_copy = sources.clone();
                let context = self.clone();
                let b = bar.clone();

                let handle = runtime.spawn_blocking(move || {
                    let package_source = source_copy.get(&source_registry).unwrap();
                    let contents = package_source.download_package(&package_id)?;
                    b.println(format!(
                        "{} Downloaded {}{}",
                        SetForegroundColor(Color::DarkGreen),
                        SetForegroundColor(Color::Reset),
                        package_id,
                    ));
                    b.inc(1);
                    context.write_contents(&package_id, &contents, package_realm, container)
                });

                handles.push(handle);
            }
        }

        let num_packages = handles.len();

        for handle in handles {
            runtime
                .block_on(handle)
                .expect("Package failed to be installed.")?;
        }

        bar.finish_and_clear();
        log::info!("Downloaded {} packages!", num_packages);

        Ok(())
    }

    /// Link from a realm root directory (e.g. `Packages/Foo.lua`) into the
    /// specified container (`_Index` or `_Workspace`).
    fn link_root(&self, id: &PackageId, container: PackageContainer) -> String {
        formatdoc! {r#"
            return require(script.Parent.{container}["{full_name}"]["{short_name}"])
            "#,
            container = container.dir_name(),
            full_name = package_id_file_name(id),
            short_name = id.name().name()
        }
    }

    /// Link between packages. When `source_container` and `target_container`
    /// are the same, the link navigates within the same container
    /// (`script.Parent.Parent`). When they differ, the link goes up to the
    /// realm root and back down into the target container.
    fn link_sibling(
        &self,
        id: &PackageId,
        source_container: PackageContainer,
        target_container: PackageContainer,
    ) -> String {
        if source_container == target_container {
            formatdoc! {r#"
                return require(script.Parent.Parent["{full_name}"]["{short_name}"])
                "#,
                full_name = package_id_file_name(id),
                short_name = id.name().name()
            }
        } else {
            formatdoc! {r#"
                return require(script.Parent.Parent.Parent.{container}["{full_name}"]["{short_name}"])
                "#,
                container = target_container.dir_name(),
                full_name = package_id_file_name(id),
                short_name = id.name().name()
            }
        }
    }

    /// Cross-realm link into the shared packages directory, targeting the
    /// specified container.
    fn link_shared(&self, id: &PackageId, container: PackageContainer) -> anyhow::Result<String> {
        let shared_path = self.shared_path.as_ref().ok_or_else(|| {
            format_err!(indoc! {r#"
                A server or dev dependency is depending on a shared dependency.
                To link these packages correctly you must declare where shared
                packages are placed in the roblox datamodel in your wally.toml.
                
                This typically looks like:

                [place]
                shared-packages = "game.ReplicatedStorage.Packages"
            "#})
        })?;

        let contents = formatdoc! {r#"
            return require({packages}.{container}["{full_name}"]["{short_name}"])
            "#,
            packages = shared_path,
            container = container.dir_name(),
            full_name = package_id_file_name(id),
            short_name = id.name().name()
        };

        Ok(contents)
    }

    /// Cross-realm link into the server packages directory, targeting the
    /// specified container.
    fn link_server(&self, id: &PackageId, container: PackageContainer) -> anyhow::Result<String> {
        let server_path = self.server_path.as_ref().ok_or_else(|| {
            format_err!(indoc! {r#"
                A dev dependency is depending on a server dependency.
                To link these packages correctly you must declare where server
                packages are placed in the roblox datamodel in your wally.toml.
                
                This typically looks like:

                [place]
                server-packages = "game.ServerScriptService.Packages"
            "#})
        })?;

        let contents = formatdoc! {r#"
            return require({packages}.{container}["{full_name}"]["{short_name}"])
            "#,
            packages = server_path,
            container = container.dir_name(),
            full_name = package_id_file_name(id),
            short_name = id.name().name()
        };

        Ok(contents)
    }

    fn write_root_package_links<'a, K: Display>(
        &self,
        root_realm: Realm,
        dependencies: impl IntoIterator<Item = (K, &'a PackageId)>,
        resolved: &Resolve,
    ) -> anyhow::Result<()> {
        log::debug!("Writing root package links");

        let base_path = match root_realm {
            Realm::Shared => &self.shared_dir,
            Realm::Server => &self.server_dir,
            Realm::Dev => &self.dev_dir,
        };

        log::trace!("Creating directory {}", base_path.display());
        fs::create_dir_all(base_path)?;

        for (dep_name, dep_package_id) in dependencies {
            let dep_meta = resolved.metadata.get(dep_package_id).unwrap();
            let dep_realm = dep_meta.origin_realm;
            let dep_container = package_container(resolved, dep_package_id);
            let path = base_path.join(format!("{}.lua", dep_name));

            let contents = match (root_realm, dep_realm) {
                (source, dest) if source == dest => {
                    self.link_root(dep_package_id, dep_container)
                }
                (_, Realm::Server) => self.link_server(dep_package_id, dep_container)?,
                (_, Realm::Shared) => self.link_shared(dep_package_id, dep_container)?,
                (_, Realm::Dev) => {
                    bail!("A dev dependency cannot be depended upon by a non-dev dependency")
                }
            };

            log::trace!("Writing {}", path.display());
            fs::write(path, contents)?;
        }

        Ok(())
    }

    fn write_package_links<'a, K: std::fmt::Display>(
        &self,
        package_id: &PackageId,
        package_realm: Realm,
        dependencies: impl IntoIterator<Item = (K, &'a PackageId)>,
        resolved: &Resolve,
    ) -> anyhow::Result<()> {
        log::debug!("Writing package links for {}", package_id);

        let source_container = package_container(resolved, package_id);
        let base_path = self
            .container_dir(package_realm, source_container)
            .join(package_id_file_name(package_id));

        log::trace!("Creating directory {}", base_path.display());
        fs::create_dir_all(&base_path)?;

        for (dep_name, dep_package_id) in dependencies {
            let dep_meta = resolved.metadata.get(dep_package_id).unwrap();
            let dep_realm = dep_meta.origin_realm;
            let dep_container = package_container(resolved, dep_package_id);
            let path = base_path.join(format!("{}.lua", dep_name));

            let contents = match (package_realm, dep_realm) {
                (source, dest) if source == dest => {
                    self.link_sibling(dep_package_id, source_container, dep_container)
                }
                (_, Realm::Server) => self.link_server(dep_package_id, dep_container)?,
                (_, Realm::Shared) => self.link_shared(dep_package_id, dep_container)?,
                (_, Realm::Dev) => {
                    bail!("A dev dependency cannot be depended upon by a non-dev dependency")
                }
            };

            log::trace!("Writing {}", path.display());
            fs::write(path, contents)?;
        }

        Ok(())
    }

    fn write_contents(
        &self,
        package_id: &PackageId,
        contents: &PackageContents,
        realm: Realm,
        container: PackageContainer,
    ) -> anyhow::Result<()> {
        let mut path = self.container_dir(realm, container).to_path_buf();

        path.push(package_id_file_name(package_id));
        path.push(package_id.name().name());

        fs::create_dir_all(&path)?;
        contents.unpack_into_path(&path)?;

        Ok(())
    }
}

/// Creates a suitable name for use in file paths that refer to this package.
fn package_id_file_name(id: &PackageId) -> String {
    format!(
        "{}_{}@{}",
        id.name().scope(),
        id.name().name(),
        id.version()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_container_dir_names() {
        assert_eq!(PackageContainer::Index.dir_name(), "_Index");
        assert_eq!(PackageContainer::Workspace.dir_name(), "_Workspace");
    }

    #[test]
    fn link_root_index() {
        let ctx = InstallationContext::new(Path::new("/project"), None, None);
        let id: PackageId = "biff/cool@1.0.0".parse().unwrap();
        let link = ctx.link_root(&id, PackageContainer::Index);
        assert!(link.contains("_Index"));
        assert!(link.contains("biff_cool@1.0.0"));
        assert!(link.contains(r#"["cool"]"#));
    }

    #[test]
    fn link_root_workspace() {
        let ctx = InstallationContext::new(Path::new("/project"), None, None);
        let id: PackageId = "team/foo@1.0.0".parse().unwrap();
        let link = ctx.link_root(&id, PackageContainer::Workspace);
        assert!(link.contains("_Workspace"));
        assert!(link.contains("team_foo@1.0.0"));
    }

    #[test]
    fn link_sibling_same_container() {
        let ctx = InstallationContext::new(Path::new("/project"), None, None);
        let id: PackageId = "biff/dep@2.0.0".parse().unwrap();
        let link = ctx.link_sibling(&id, PackageContainer::Index, PackageContainer::Index);
        assert!(link.contains("script.Parent.Parent"));
        assert!(!link.contains("_Index"));
        assert!(!link.contains("_Workspace"));
    }

    #[test]
    fn link_sibling_cross_container_workspace_to_index() {
        let ctx = InstallationContext::new(Path::new("/project"), None, None);
        let id: PackageId = "ext/util@1.0.0".parse().unwrap();
        let link = ctx.link_sibling(&id, PackageContainer::Workspace, PackageContainer::Index);
        assert!(link.contains("script.Parent.Parent.Parent._Index"));
        assert!(link.contains("ext_util@1.0.0"));
    }

    #[test]
    fn link_sibling_cross_container_index_to_workspace() {
        let ctx = InstallationContext::new(Path::new("/project"), None, None);
        let id: PackageId = "team/foo@1.0.0".parse().unwrap();
        let link = ctx.link_sibling(&id, PackageContainer::Index, PackageContainer::Workspace);
        assert!(link.contains("script.Parent.Parent.Parent._Workspace"));
        assert!(link.contains("team_foo@1.0.0"));
    }

    #[test]
    fn link_shared_with_container() {
        let ctx = InstallationContext::new(
            Path::new("/project"),
            Some("game.ReplicatedStorage.Packages".to_string()),
            None,
        );
        let id: PackageId = "biff/shared@1.0.0".parse().unwrap();

        let link_index = ctx.link_shared(&id, PackageContainer::Index).unwrap();
        assert!(link_index.contains("game.ReplicatedStorage.Packages._Index"));

        let link_ws = ctx.link_shared(&id, PackageContainer::Workspace).unwrap();
        assert!(link_ws.contains("game.ReplicatedStorage.Packages._Workspace"));
    }

    #[test]
    fn link_server_with_container() {
        let ctx = InstallationContext::new(
            Path::new("/project"),
            None,
            Some("game.ServerScriptService.Packages".to_string()),
        );
        let id: PackageId = "biff/server@1.0.0".parse().unwrap();

        let link_index = ctx.link_server(&id, PackageContainer::Index).unwrap();
        assert!(link_index.contains("game.ServerScriptService.Packages._Index"));

        let link_ws = ctx.link_server(&id, PackageContainer::Workspace).unwrap();
        assert!(link_ws.contains("game.ServerScriptService.Packages._Workspace"));
    }

    #[test]
    fn link_shared_missing_path_errors() {
        let ctx = InstallationContext::new(Path::new("/project"), None, None);
        let id: PackageId = "biff/shared@1.0.0".parse().unwrap();
        assert!(ctx.link_shared(&id, PackageContainer::Index).is_err());
    }

    #[test]
    fn link_server_missing_path_errors() {
        let ctx = InstallationContext::new(Path::new("/project"), None, None);
        let id: PackageId = "biff/server@1.0.0".parse().unwrap();
        assert!(ctx.link_server(&id, PackageContainer::Index).is_err());
    }

    #[test]
    fn container_dir_returns_correct_paths() {
        let ctx = InstallationContext::new(Path::new("/project"), None, None);

        assert_eq!(
            ctx.container_dir(Realm::Shared, PackageContainer::Index),
            Path::new("/project/Packages/_Index")
        );
        assert_eq!(
            ctx.container_dir(Realm::Shared, PackageContainer::Workspace),
            Path::new("/project/Packages/_Workspace")
        );
        assert_eq!(
            ctx.container_dir(Realm::Server, PackageContainer::Index),
            Path::new("/project/ServerPackages/_Index")
        );
        assert_eq!(
            ctx.container_dir(Realm::Server, PackageContainer::Workspace),
            Path::new("/project/ServerPackages/_Workspace")
        );
        assert_eq!(
            ctx.container_dir(Realm::Dev, PackageContainer::Index),
            Path::new("/project/DevPackages/_Index")
        );
        assert_eq!(
            ctx.container_dir(Realm::Dev, PackageContainer::Workspace),
            Path::new("/project/DevPackages/_Workspace")
        );
    }

    #[test]
    fn package_container_from_resolve_metadata() {
        use crate::package_source::PackageSourceId;
        use crate::resolution::ResolvePackageMetadata;

        let id_registry: PackageId = "ext/util@1.0.0".parse().unwrap();
        let id_workspace: PackageId = "team/foo@1.0.0".parse().unwrap();

        let mut resolve = Resolve::default();
        resolve.metadata.insert(
            id_registry.clone(),
            ResolvePackageMetadata {
                realm: Realm::Shared,
                origin_realm: Realm::Shared,
                source_registry: PackageSourceId::DefaultRegistry,
                is_workspace_member: false,
            },
        );
        resolve.metadata.insert(
            id_workspace.clone(),
            ResolvePackageMetadata {
                realm: Realm::Shared,
                origin_realm: Realm::Shared,
                source_registry: PackageSourceId::Path("/ws/modules/foo".into()),
                is_workspace_member: true,
            },
        );

        assert_eq!(
            package_container(&resolve, &id_registry),
            PackageContainer::Index
        );
        assert_eq!(
            package_container(&resolve, &id_workspace),
            PackageContainer::Workspace
        );
    }
}
