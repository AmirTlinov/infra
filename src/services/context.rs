use crate::errors::ToolError;
use crate::utils::fs_atomic::path_exists;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const MARKERS: &[(&str, &[&str])] = &[
    ("node", &["package.json", "pnpm-lock.yaml", "yarn.lock"]),
    (
        "python",
        &["pyproject.toml", "requirements.txt", "Pipfile", "setup.py"],
    ),
    ("go", &["go.mod"]),
    ("rust", &["Cargo.toml"]),
    ("java", &["pom.xml", "build.gradle", "build.gradle.kts"]),
    ("dotnet", &["global.json"]),
    (
        "docker",
        &["Dockerfile", "docker-compose.yml", "docker-compose.yaml"],
    ),
    (
        "k8s",
        &["kustomization.yaml", "kustomization.yml", "Kustomization"],
    ),
    ("helm", &["Chart.yaml"]),
    (
        "argocd",
        &[".argocd", "argocd-application.yaml", "Application.yaml"],
    ),
    (
        "flux",
        &[
            ".flux",
            "flux-system",
            "gotk-components.yaml",
            "gotk-sync.yaml",
            "flux-system/gotk-components.yaml",
            "flux-system/gotk-sync.yaml",
            "flux-system/kustomization.yaml",
        ],
    ),
    ("terraform", &["main.tf", "terraform.tf", "terragrunt.hcl"]),
    ("ansible", &["ansible.cfg", "playbook.yml", "playbook.yaml"]),
    ("ci", &[".github/workflows", "gitlab-ci.yml", "Jenkinsfile"]),
];

#[derive(Clone, Default)]
pub struct ContextService;

impl ContextService {
    pub fn new() -> Result<Self, ToolError> {
        Ok(Self)
    }

    async fn detect_markers(&self, root: &Path) -> (HashMap<String, bool>, HashMap<String, bool>) {
        let mut files = HashMap::new();
        let mut signals = HashMap::new();
        for (tag, entries) in MARKERS.iter() {
            let mut hit = false;
            for rel in *entries {
                let full = if Path::new(rel).is_absolute() {
                    PathBuf::from(rel)
                } else {
                    root.join(rel)
                };
                let exists = path_exists(full);
                files.insert(rel.to_string(), exists);
                if exists {
                    hit = true;
                }
            }
            signals.insert(tag.to_string(), hit);
        }
        (files, signals)
    }

    fn derive_tags(&self, signals: &HashMap<String, bool>, git_root: Option<&Path>) -> Vec<String> {
        let mut tags: Vec<String> = signals
            .iter()
            .filter(|(_, value)| **value)
            .map(|(key, _)| key.clone())
            .collect();
        if *signals.get("argocd").unwrap_or(&false) || *signals.get("flux").unwrap_or(&false) {
            tags.push("gitops".to_string());
        }
        if git_root.is_some() {
            tags.push("git".to_string());
        }
        tags.sort();
        tags
    }

    fn find_git_root(&self, start: &Path) -> Option<PathBuf> {
        let mut current = start.to_path_buf();
        for _ in 0..25 {
            if current.join(".git").exists() {
                return Some(current);
            }
            if let Some(parent) = current.parent() {
                if parent == current {
                    break;
                }
                current = parent.to_path_buf();
            } else {
                break;
            }
        }
        None
    }

    pub async fn get_context(&self, args: &Value) -> Result<Value, ToolError> {
        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let refresh = args
            .get("refresh")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let cwd = args
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let repo_root = args
            .get("repo_root")
            .and_then(|v| v.as_str())
            .map(PathBuf::from);
        let project_name = args
            .get("project")
            .or_else(|| args.get("project_name"))
            .and_then(|v| v.as_str());
        let target_name = args
            .get("target")
            .or_else(|| args.get("project_target"))
            .and_then(|v| v.as_str());

        let key = key.unwrap_or_else(|| {
            if let Some(project) = project_name {
                format!("project:{}:{}", project, target_name.unwrap_or("default"))
            } else {
                format!("cwd:{}", cwd.display())
            }
        });

        let git_root = self.find_git_root(&cwd);
        let root = repo_root
            .as_ref()
            .or(git_root.as_ref())
            .unwrap_or(&cwd)
            .clone();
        let (files, signals) = self.detect_markers(&root).await;
        let tags = self.derive_tags(&signals, git_root.as_deref());

        let payload = serde_json::json!({
            "key": key,
            "root": root,
            "cwd": cwd,
            "project_name": project_name,
            "target_name": target_name,
            "repo_root": repo_root,
            "git": git_root.as_ref().map(|root| serde_json::json!({"root": root})),
            "tags": tags,
            "signals": signals,
            "files": files,
            "updated_at": chrono::Utc::now().to_rfc3339(),
        });

        let _ = refresh;
        Ok(serde_json::json!({"success": true, "context": payload}))
    }
}
