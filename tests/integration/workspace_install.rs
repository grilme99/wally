use std::collections::BTreeSet;
use std::path::Path;

use libwally::{
    installation::InstallationContext,
    lockfile::Lockfile,
    package_id::PackageId,
    package_source::{PackageSource, PackageSourceMap, TestRegistry},
    resolution::resolve_workspace,
    workspace::Workspace,
};

use super::temp_project::TempProject;

fn run_workspace_install_test(name: &str) -> TempProject {
    let source_project =
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/test-projects")).join(name);
    let project = TempProject::new(&source_project).unwrap();

    let workspace = Workspace::load(project.path()).unwrap();

    let registry_path = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test-registries/primary-registry"
    ));
    let default_registry: Box<PackageSource> =
        Box::new(PackageSource::TestRegistry(TestRegistry::new(registry_path)));
    let mut sources = PackageSourceMap::new(default_registry);
    sources.add_fallbacks().unwrap();

    let resolved = resolve_workspace(&workspace, &Default::default(), &sources).unwrap();

    Lockfile::from_resolve(&resolved, Some(workspace.root()))
        .save(project.path())
        .unwrap();

    let root_ids: BTreeSet<PackageId> = workspace
        .members()
        .values()
        .map(|m| m.package_id())
        .collect();

    let ctx = InstallationContext::new(
        project.path(),
        workspace.place().shared_packages.clone(),
        workspace.place().server_packages.clone(),
    );
    ctx.clean().unwrap();
    ctx.install(sources, root_ids, resolved).unwrap();

    assert_dir_snapshot!(project.path());
    project
}

#[test]
fn workspace_virtual() {
    run_workspace_install_test("workspace-virtual");
}

#[test]
fn workspace_path_dep() {
    run_workspace_install_test("workspace-path-dep");
}

#[test]
fn workspace_with_registry_dep() {
    run_workspace_install_test("workspace-with-registry-dep");
}

#[test]
fn workspace_inheritance() {
    run_workspace_install_test("workspace-inheritance");
}

#[test]
fn workspace_hybrid_root() {
    run_workspace_install_test("workspace-hybrid-root");
}

#[test]
fn workspace_cross_realm() {
    run_workspace_install_test("workspace-cross-realm");
}
