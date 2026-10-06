//! Pi skill discovery. Only configured roots are traversed; bodies stay on the
//! host until invoked. Pi's native skill commands use the explicit skill: namespace.
use crate::HarnessError;
use paku_proto::{HarnessId, invocation::Skill};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

const MAX_DISCOVERY_ENTRIES: usize = 4096;

/// Share overlapping command probes, but refresh when a picker opens later.
#[derive(Default)]
pub(crate) struct CommandDiscovery {
    latest:
        tokio::sync::Mutex<Option<(PathBuf, std::time::Instant, Vec<paku_proto::SlashCommand>)>>,
}
impl CommandDiscovery {
    pub(crate) async fn get(
        &self,
        cwd: &Path,
        discover: impl std::future::Future<Output = Result<Vec<paku_proto::SlashCommand>, HarnessError>>,
    ) -> Result<Vec<paku_proto::SlashCommand>, HarnessError> {
        let requested = std::time::Instant::now();
        let mut latest = self.latest.lock().await;
        if let Some((root, completed, commands)) = latest.as_ref()
            && root == cwd
            && *completed >= requested
        {
            return Ok(commands.clone());
        }
        let commands = discover.await?;
        *latest = Some((cwd.to_owned(), std::time::Instant::now(), commands.clone()));
        Ok(commands)
    }
}

pub(crate) async fn discover(cwd: &Path, agent_dir: PathBuf) -> Result<Vec<Skill>, HarnessError> {
    let cwd = cwd.to_path_buf();
    let home = crate::executable::home_or_current_dir();
    tokio::task::spawn_blocking(move || discover_at(&cwd, &home, &agent_dir))
        .await
        .map_err(|error| HarnessError::Protocol(error.to_string()))?
}

pub(crate) fn attach_advertised_commands(
    skills: &mut Vec<Skill>,
    commands: &[paku_proto::SlashCommand],
) {
    use paku_proto::invocation::SkillCommand;
    for command in commands {
        if !paku_proto::invocation::valid_skill_command_name(&command.name) {
            continue;
        }
        let Some(name) = command
            .name
            .strip_prefix("skill:")
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        let invocation = SkillCommand {
            name: command.name.clone(),
            harness: HarnessId::Pi,
        };
        if let Some(skill) = skills.iter_mut().find(|skill| skill.name == name) {
            skill.command = Some(invocation);
        } else {
            skills.push(Skill {
                name: name.into(),
                path: format!("harness-skill:pi:{name}"),
                description: command.description.clone(),
                enabled: true,
                command: Some(invocation),
            });
        }
    }
}

const PROJECT_DIRS: &[&str] = &[".agents/skills", ".pi/skills"];
fn discover_at(cwd: &Path, home: &Path, agent_dir: &Path) -> Result<Vec<Skill>, HarnessError> {
    let mut roots: Vec<PathBuf> = PROJECT_DIRS.iter().map(|dir| home.join(dir)).collect();
    roots.push(agent_dir.join("skills"));
    // Stop at the nearest worktree root. Nested project skills override ancestors,
    // and project skills override global skills with the same name.
    let mut ancestors = Vec::new();
    for dir in cwd.ancestors() {
        ancestors.push(dir);
        if dir.join(".git").exists() || dir == home {
            break;
        }
    }
    for dir in ancestors.into_iter().rev() {
        roots.extend(PROJECT_DIRS.iter().map(|suffix| dir.join(suffix)));
    }
    let mut found = BTreeMap::new();
    let mut remaining = MAX_DISCOVERY_ENTRIES;
    for root in roots {
        if remaining == 0 {
            break;
        }
        scan_root(&root, &mut found, &mut remaining)?;
    }
    Ok(found.into_values().collect())
}

fn scan_root(
    root: &Path,
    found: &mut BTreeMap<String, Skill>,
    remaining: &mut usize,
) -> Result<(), HarnessError> {
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    let mut seen = HashSet::new();
    while let Some((dir, depth)) = pending.pop() {
        if *remaining == 0 {
            break;
        }
        if depth > 12 {
            continue;
        }
        let canonical = match std::fs::canonicalize(&dir) {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                tracing::warn!(path = %dir.display(), %error, "skipping inaccessible skill directory");
                continue;
            }
        };
        if !seen.insert(canonical) {
            continue;
        }
        let children = match std::fs::read_dir(&dir) {
            Ok(children) => children,
            Err(error) => {
                tracing::warn!(path = %dir.display(), %error, "skipping unreadable skill directory");
                continue;
            }
        };
        // Charge entries before filtering, allocation or sorting. All roots share the budget.
        let mut children: Vec<_> = children.take(*remaining).inspect(|_| *remaining -= 1)
            .filter_map(|entry| match entry {
                Ok(entry) => Some(entry),
                Err(error) => { tracing::warn!(path = %dir.display(), %error, "skipping unreadable skill directory entry"); None }
            }).collect();
        children.sort_by_key(|entry| entry.file_name());
        for entry in children {
            let path = entry.path();
            if path.is_dir() {
                pending.push((path, depth + 1));
                continue;
            }
            if entry.file_name() != "SKILL.md"
                && !(depth == 0 && path.extension().is_some_and(|ext| ext == "md"))
            {
                continue;
            }
            match read_skill(&path) {
                Ok(Some(skill)) => {
                    found.insert(skill.name.clone(), skill);
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "skipping unreadable skill");
                }
            }
        }
    }
    Ok(())
}

fn read_skill(path: &Path) -> Result<Option<Skill>, HarnessError> {
    use std::io::Read;
    // Support regular-file symlinks, but never open devices/pipes.
    if !std::fs::metadata(path)?.is_file() {
        return Ok(None);
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(256 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 256 * 1024 {
        return Ok(None);
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(None);
    };
    let mut lines = text.lines();
    let metadata = if lines.next().is_some_and(|line| line.trim() == "---") {
        let mut yaml = String::new();
        let mut closed = false;
        for line in lines {
            if line.trim() == "---" {
                closed = true;
                break;
            }
            yaml.push_str(line);
            yaml.push('\n');
        }
        if !closed {
            return Ok(None);
        }
        match serde_yaml_ng::from_str::<serde_json::Value>(&yaml) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        }
    } else {
        serde_json::Value::Null
    };
    let fallback = if path.file_name().is_some_and(|name| name == "SKILL.md") {
        path.parent().and_then(Path::file_name)
    } else {
        path.file_stem()
    }
    .and_then(|name| name.to_str())
    .unwrap_or_default();
    let name = metadata
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(fallback);
    let path = path.to_string_lossy();
    if !paku_proto::invocation::valid_invocation_name(name)
        || !paku_proto::invocation::valid_skill_path(&path)
    {
        return Ok(None);
    }
    Ok(Some(Skill {
        name: name.into(),
        path: path.into_owned(),
        description: metadata
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim()
            .into(),
        enabled: metadata
            .get("user-invocable")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        command: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write(root: &Path, relative: &str, body: &str) -> PathBuf {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        path
    }
    fn scan(root: &Path) -> Vec<String> {
        let mut found = BTreeMap::new();
        scan_root(root, &mut found, &mut MAX_DISCOVERY_ENTRIES.clone()).unwrap();
        found.into_keys().collect()
    }
    #[tokio::test]
    async fn command_discovery_shares_overlap_but_refreshes_later_and_other_roots() {
        let discovery = CommandDiscovery::default();
        let probes = std::cell::Cell::new(0);
        let probe = || async {
            probes.set(probes.get() + 1);
            tokio::task::yield_now().await;
            Ok(vec![paku_proto::SlashCommand {
                name: format!("probe-{}", probes.get()),
                description: String::new(),
                input_hint: None,
            }])
        };
        let root = Path::new("/workspace");
        let (a, b) = tokio::join!(discovery.get(root, probe()), discovery.get(root, probe()));
        assert_eq!(a.unwrap()[0].name, b.unwrap()[0].name);
        assert_eq!(probes.get(), 1);
        assert_eq!(
            discovery.get(root, probe()).await.unwrap()[0].name,
            "probe-2"
        );
        let (a, b) = tokio::join!(
            discovery.get(root, probe()),
            discovery.get(Path::new("/other"), probe())
        );
        assert_ne!(a.unwrap()[0].name, b.unwrap()[0].name);
        assert_eq!(probes.get(), 4);
        assert!(
            discovery
                .get(root, async { Err(HarnessError::Protocol("retry".into())) })
                .await
                .is_err()
        );
        assert_eq!(
            discovery.get(root, probe()).await.unwrap()[0].name,
            "probe-5"
        );
    }
    #[test]
    fn pi_roots_precedence_agent_override_and_git_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        let nested = repo.join("packages/web");
        let agent = temp.path().join("custom-agent");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        write(
            &home,
            ".agents/skills/review/SKILL.md",
            "---\nname: review\ndescription: Global\n---\nBody",
        );
        write(
            &repo,
            ".agents/skills/review/SKILL.md",
            "---\nname: review\ndescription: Project\n---\nBody",
        );
        let native = write(
            &nested,
            ".pi/skills/review/SKILL.md",
            "---\nname: review\ndescription: >-\n  Native multiline\n  description\ndisable-model-invocation: true\n---\nBody",
        );
        write(&agent, "skills/custom/SKILL.md", "Body");
        write(&home, ".pi/agent/skills/not-selected/SKILL.md", "Body");
        write(temp.path(), ".agents/skills/outside/SKILL.md", "Body");
        write(&repo, ".claude/skills/removed/SKILL.md", "Body");
        write(
            &repo,
            ".pi/skills/hidden/SKILL.md",
            "---\nuser-invocable: false\n---\nBody",
        );
        let skills = discover_at(&nested, &home, &agent).unwrap();
        assert_eq!(skills.len(), 3);
        let review = skills.iter().find(|s| s.name == "review").unwrap();
        assert_eq!(Path::new(&review.path), native);
        assert_eq!(review.description, "Native multiline description");
        assert!(review.enabled);
        assert!(skills.iter().any(|s| s.name == "custom"));
        assert!(!skills.iter().find(|s| s.name == "hidden").unwrap().enabled);
    }
    #[test]
    fn pi_skill_commands_cannot_alias_builtin_commands() {
        let mut skills = vec![Skill {
            name: "compact".into(),
            path: "/repo/.agents/skills/compact/SKILL.md".into(),
            description: "Compact JSON".into(),
            enabled: true,
            command: None,
        }];
        let command = |name: &str| paku_proto::SlashCommand {
            name: name.into(),
            description: String::new(),
            input_hint: None,
        };
        attach_advertised_commands(&mut skills, &[command("compact")]);
        assert!(skills[0].command.is_none());
        attach_advertised_commands(
            &mut skills,
            &[command("skill:compact"), command("skill:review")],
        );
        assert_eq!(skills[0].command.as_ref().unwrap().name, "skill:compact");
        assert_eq!(skills[1].path, "harness-skill:pi:review");
        let skill = &skills[1];
        let invocation = paku_proto::invocation::Invocation::Skill {
            name: skill.name.clone(),
            path: skill.path.clone(),
            command: skill.command.clone(),
        };
        assert_eq!(
            paku_proto::invocation::harness_prompt(
                &format!("{} arguments", invocation.link()),
                HarnessId::Pi
            ),
            "/skill:review arguments"
        );
    }
    #[test]
    fn advertised_commands_preserve_valid_skill_links() {
        let commands = [
            "skill:",
            "skill:two words",
            "skill:bad\nname",
            "skill:bad\0name",
            "skill:review[ui]",
            "skill:审查-é:ui.v2_test",
        ]
        .into_iter()
        .map(|name| paku_proto::SlashCommand {
            name: name.into(),
            description: String::new(),
            input_hint: None,
        })
        .collect::<Vec<_>>();
        let mut skills = vec![];
        attach_advertised_commands(&mut skills, &commands);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "审查-é:ui.v2_test");
    }
    #[test]
    fn malformed_and_oversized_skills_do_not_hide_valid_completions() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), "valid/SKILL.md", "---\nname: valid\n---\nBody");
        let invalid = write(temp.path(), "invalid/SKILL.md", "");
        std::fs::write(invalid, [0xff, 0xfe]).unwrap();
        write(
            temp.path(),
            "oversized/SKILL.md",
            &format!("{}é", "x".repeat(256 * 1024)),
        );
        write(
            temp.path(),
            "broken/SKILL.md",
            "---\nname: [broken\n---\nBody",
        );
        assert_eq!(scan(temp.path()), ["valid"]);
        for name in [
            "two words",
            " padded",
            "padded ",
            "tab\tname",
            "non\u{a0}breaking",
        ] {
            let path = write(
                temp.path(),
                "invalid/SKILL.md",
                &format!(
                    "---\nname: {}\n---\nBody",
                    serde_json::to_string(name).unwrap()
                ),
            );
            assert!(read_skill(&path).unwrap().is_none());
        }
    }
    #[test]
    fn discovery_caps_large_directories_across_roots() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        write(&home, ".agents/skills/global.md", "Global");
        for index in 0..4200 {
            write(&repo, &format!(".pi/skills/skill-{index}.md"), "Body");
        }
        let skills = discover_at(&repo, &home, &home.join(".pi/agent")).unwrap();
        assert_eq!(skills.len(), MAX_DISCOVERY_ENTRIES);
        assert!(skills.iter().any(|s| s.name == "global"));
        let mut found = BTreeMap::new();
        let mut remaining = 1;
        scan_root(&repo, &mut found, &mut remaining).unwrap();
        assert_eq!(remaining, 0);
        assert!(found.is_empty());
    }
    #[cfg(unix)]
    #[test]
    fn symlinks_work_without_following_cycles_or_hiding_valid_skills() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("skills");
        let source = write(temp.path(), "source/review/SKILL.md", "Body");
        write(&root, "valid.md", "Body");
        symlink(source.parent().unwrap(), root.join("review")).unwrap();
        symlink(&root, root.join("cycle")).unwrap();
        symlink(temp.path().join("missing"), root.join("missing.md")).unwrap();
        let denied = write(&root, "private/SKILL.md", "Body");
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0)).unwrap();
        let readable = std::fs::File::open(&denied).is_ok();
        let found = scan(&root);
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(found.contains(&"valid".to_owned()));
        assert!(found.contains(&"review".to_owned()));
        assert_eq!(found.contains(&"private".to_owned()), readable);
        assert!(!found.contains(&"missing".to_owned()));
    }
    #[cfg(unix)]
    #[test]
    fn discovery_skips_special_files_without_blocking() {
        const CHILD: &str = "PAKU_SKILL_SPECIAL_FILE_TEST";
        if std::env::var_os(CHILD).is_none() {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "skills::tests::discovery_skips_special_files_without_blocking",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success());
                    return;
                }
                if std::time::Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("skill discovery blocked");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        use std::os::unix::{fs::symlink, net::UnixListener};
        let temp = tempfile::tempdir().unwrap();
        let valid = write(temp.path(), "valid.md", "Body");
        symlink(valid, temp.path().join("linked.md")).unwrap();
        let fifo = temp.path().join("pipe.md");
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        symlink(fifo, temp.path().join("pipe-link.md")).unwrap();
        symlink("/dev/zero", temp.path().join("device.md")).unwrap();
        let _socket = UnixListener::bind(temp.path().join("socket.md")).unwrap();
        assert_eq!(scan(temp.path()), ["linked", "valid"]);
    }
}
