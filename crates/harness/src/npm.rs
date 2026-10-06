//! npm discovery for explicit Pi installation (never used during a run).
use crate::executable::Platform;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

fn npm_cli_for_node(node: &Path) -> Option<PathBuf> {
    let cli = node.parent()?.join("node_modules/npm/bin/npm-cli.js");
    cli.is_file().then_some(cli)
}

fn find_npm_with(
    env: &impl Fn(&str) -> Option<OsString>,
    login_shell_path: Option<OsString>,
    platform: Platform,
) -> Option<PathBuf> {
    if platform == Platform::Windows {
        let node = crate::executable::find_on_paths_matching_with(
            "node",
            Vec::new(),
            env,
            login_shell_path,
            platform,
            |node| npm_cli_for_node(node).is_some(),
        )?;
        npm_cli_for_node(&node)
    } else {
        crate::executable::find_on_paths_with("npm", Vec::new(), env, login_shell_path, platform)
    }
}

pub(crate) fn find_npm() -> Option<PathBuf> {
    find_npm_with(
        &|key| std::env::var_os(key),
        crate::shell_env::login_shell_path().map(OsString::from),
        Platform::current(),
    )
}

/// Windows uses node + npm-cli.js rather than a shell wrapper.
pub(crate) fn node_for_npm(npm: &Path) -> Option<PathBuf> {
    npm.parent()?
        .parent()?
        .parent()?
        .parent()
        .map(|root| root.join("node.exe"))
        .filter(|node| node.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_npm_uses_the_matching_native_node_installation() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("node-root");
        let cli = bin.join("node_modules/npm/bin/npm-cli.js");
        std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
        std::fs::write(&cli, "// npm").unwrap();
        let node = bin.join("node.exe");
        std::fs::write(&node, "fixture").unwrap();
        let env = |key: &str| match key {
            "PATH" => Some(std::env::join_paths([&bin]).unwrap()),
            "PATHEXT" => Some(OsString::from(".exe;.cmd")),
            _ => None,
        };
        assert_eq!(
            find_npm_with(&env, None, Platform::Windows),
            Some(cli.clone())
        );
        assert_eq!(node_for_npm(&cli), Some(node));
        std::fs::remove_file(cli).unwrap();
        assert!(find_npm_with(&env, None, Platform::Windows).is_none());
    }
}
