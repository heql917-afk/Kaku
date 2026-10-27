//! Quick Commands: user-defined command snippets shown in a tab-bar button.
//!
//! Data model, JSON persistence in the Kaku config dir, and pure helpers
//! shared by the overlay (`overlay/quick_commands.rs`).

use anyhow::Context;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QuickCommand {
    pub label: String,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QuickCommandGroup {
    pub name: String,
    pub commands: Vec<QuickCommand>,
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct QuickCommandsFile {
    pub groups: Vec<QuickCommandGroup>,
}

/// One line of the overlay list: a group heading or a command entry.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Group { name: String },
    Command {
        group_idx: usize,
        cmd_idx: usize,
        label: String,
        command: String,
    },
}

pub fn quick_commands_path() -> std::path::PathBuf {
    config::CONFIG_DIRS
        .first()
        .cloned()
        .unwrap_or_else(|| config::HOME_DIR.join(".config").join("kaku"))
        .join("quick-commands.json")
}

pub fn default_quick_commands() -> QuickCommandsFile {
    QuickCommandsFile {
        groups: vec![
            QuickCommandGroup {
                name: "日常".to_string(),
                commands: vec![QuickCommand {
                    label: "启动控制台".to_string(),
                    command: "./start.sh".to_string(),
                }],
            },
            QuickCommandGroup {
                name: "测试".to_string(),
                commands: vec![
                    QuickCommand {
                        label: "vitest 单测".to_string(),
                        command: "npx vitest run".to_string(),
                    },
                    QuickCommand {
                        label: "cargo 测试".to_string(),
                        command: "cargo test".to_string(),
                    },
                ],
            },
        ],
    }
}

/// Strip CR/LF so an inserted command can never execute itself.
pub fn sanitize_command(s: &str) -> String {
    s.chars().filter(|&c| c != '\r' && c != '\n').collect()
}

pub fn load_from(path: &std::path::Path) -> QuickCommandsFile {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(default_quick_commands)
}

pub fn save_to(path: &std::path::Path, file: &QuickCommandsFile) -> anyhow::Result<()> {
    // Empty groups carry no information; don't persist them.
    let mut cleaned = file.clone();
    cleaned.groups.retain(|g| !g.commands.is_empty());

    if let Some(parent) = path.parent() {
        config::create_user_owned_dirs(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }

    let encoded =
        serde_json::to_string_pretty(&cleaned).context("encode quick commands")?;
    // Atomic write: temp file in the same directory, then rename on top,
    // so a crash mid-write can't leave a truncated JSON file behind.
    let tmp = path.with_file_name(format!(
        "{}.{}.{}.tmp",
        path.file_stem().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&tmp, format!("{encoded}\n"))
        .with_context(|| format!("write {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("rename to {}", path.display()));
    }
    Ok(())
}

impl QuickCommandsFile {
    /// Rows for the overlay list. With an empty filter every group heading
    /// and command is listed in order; with a filter, only groups containing
    /// a fuzzy match on label or command survive.
    pub fn rows(&self, filter: &str) -> Vec<Row> {
        let mut rows = vec![];
        if filter.is_empty() {
            for (group_idx, group) in self.groups.iter().enumerate() {
                rows.push(Row::Group {
                    name: group.name.clone(),
                });
                for (cmd_idx, cmd) in group.commands.iter().enumerate() {
                    rows.push(Row::Command {
                        group_idx,
                        cmd_idx,
                        label: cmd.label.clone(),
                        command: cmd.command.clone(),
                    });
                }
            }
        } else {
            let pattern = crate::overlay::selector::matcher_pattern(filter);
            for (group_idx, group) in self.groups.iter().enumerate() {
                let mut matched = vec![];
                for (cmd_idx, cmd) in group.commands.iter().enumerate() {
                    let hit = crate::overlay::selector::matcher_score(&pattern, &cmd.label)
                        .is_some()
                        || crate::overlay::selector::matcher_score(&pattern, &cmd.command)
                            .is_some();
                    if hit {
                        matched.push(Row::Command {
                            group_idx,
                            cmd_idx,
                            label: cmd.label.clone(),
                            command: cmd.command.clone(),
                        });
                    }
                }
                if !matched.is_empty() {
                    rows.push(Row::Group {
                        name: group.name.clone(),
                    });
                    rows.extend(matched);
                }
            }
        }
        rows
    }

    /// Append a command, creating the group when it doesn't exist yet.
    pub fn insert_command(&mut self, group: &str, label: &str, command: &str) {
        let entry = QuickCommand {
            label: label.to_string(),
            command: sanitize_command(command),
        };
        let name = group.trim();
        let name = if name.is_empty() { "日常" } else { name };
        if let Some(existing) = self.groups.iter_mut().find(|g| g.name == name) {
            existing.commands.push(entry);
        } else {
            self.groups.push(QuickCommandGroup {
                name: name.to_string(),
                commands: vec![entry],
            });
        }
    }

    pub fn update_command(
        &mut self,
        group_idx: usize,
        cmd_idx: usize,
        label: &str,
        command: &str,
    ) -> bool {
        let Some(group) = self.groups.get_mut(group_idx) else {
            return false;
        };
        let Some(cmd) = group.commands.get_mut(cmd_idx) else {
            return false;
        };
        cmd.label = label.to_string();
        cmd.command = sanitize_command(command);
        true
    }

    pub fn delete_command(&mut self, group_idx: usize, cmd_idx: usize) -> bool {
        let Some(group) = self.groups.get_mut(group_idx) else {
            return false;
        };
        if cmd_idx >= group.commands.len() {
            return false;
        }
        group.commands.remove(cmd_idx);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_file_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.json");
        assert_eq!(load_from(&path), default_quick_commands());
    }

    #[test]
    fn corrupt_json_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.json");
        std::fs::write(&path, b"not json{").unwrap();
        assert_eq!(load_from(&path), default_quick_commands());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("quick-commands.json");
        let mut file = QuickCommandsFile::default();
        file.insert_command("工作", "构建", "make build");
        file.insert_command("工作", "发布", "make release");
        file.insert_command("临时", "清缓存", "rm -rf target");
        // An empty group must not survive the round trip.
        file.groups.push(QuickCommandGroup {
            name: "空组".to_string(),
            commands: vec![],
        });
        save_to(&path, &file).unwrap();
        let loaded = load_from(&path);
        assert!(loaded.groups.iter().all(|g| !g.commands.is_empty()));
        let mut expected = file;
        expected.groups.retain(|g| !g.commands.is_empty());
        assert_eq!(loaded, expected);
    }

    #[test]
    fn sanitize_strips_newlines() {
        assert_eq!(sanitize_command("a\r\nb\nc"), "abc");
        assert_eq!(sanitize_command("echo 'hello world'"), "echo 'hello world'");
    }

    #[test]
    fn rows_without_filter_lists_groups_and_commands() {
        let file = default_quick_commands();
        let rows = file.rows("");
        assert_eq!(rows.len(), 5); // 2 group headings + 3 commands
        assert_eq!(rows[0], Row::Group { name: "日常".to_string() });
        assert_eq!(
            rows[1],
            Row::Command {
                group_idx: 0,
                cmd_idx: 0,
                label: "启动控制台".to_string(),
                command: "./start.sh".to_string(),
            }
        );
        assert_eq!(rows[2], Row::Group { name: "测试".to_string() });
    }

    #[test]
    fn rows_with_filter_keeps_only_matching_groups() {
        let file = default_quick_commands();
        let rows = file.rows("vitest");
        assert_eq!(rows.len(), 2); // group heading + the one matching command
        assert_eq!(rows[0], Row::Group { name: "测试".to_string() });
        assert!(matches!(&rows[1], Row::Command { label, .. } if label == "vitest 单测"));
    }

    #[test]
    fn insert_creates_missing_group() {
        let mut file = QuickCommandsFile::default();
        let before = file.groups.len();
        file.insert_command("新组", "命令", "echo hi");
        assert_eq!(file.groups.len(), before + 1);
        let group = file.groups.last().unwrap();
        assert_eq!(group.name, "新组");
        assert_eq!(group.commands[0].command, "echo hi");
    }

    #[test]
    fn update_and_delete_by_index() {
        let mut file = default_quick_commands();
        assert!(file.update_command(0, 0, "新名字", "make build"));
        assert_eq!(file.groups[0].commands[0].label, "新名字");
        assert_eq!(file.groups[0].commands[0].command, "make build");
        assert!(!file.update_command(99, 0, "x", "y"));
        assert!(!file.update_command(0, 99, "x", "y"));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quick-commands.json");
        // Delete the only command of group 0, then save: the empty group
        // must not reach disk.
        assert!(file.delete_command(0, 0));
        assert!(!file.delete_command(0, 0));
        save_to(&path, &file).unwrap();
        let loaded = load_from(&path);
        assert!(loaded.groups.iter().all(|g| g.name != "日常"));
    }
}
