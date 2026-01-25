use crate::errors::ToolError;
use crate::services::alias::AliasService;
use crate::services::capability::CapabilityService;
use crate::services::context::ContextService;
use crate::services::context_session::ContextSessionService;
use crate::services::logger::Logger;
use crate::services::preset::PresetService;
use crate::services::profile::ProfileService;
use crate::services::project::ProjectService;
use crate::services::project_resolver::ProjectResolver;
use crate::services::runbook::RunbookService;
use crate::services::state::StateService;
use crate::utils::data_path::get_path_value;
use crate::utils::fs_atomic::path_exists;
use crate::utils::listing::ListFilters;
use crate::utils::paths::{
    resolve_aliases_path, resolve_audit_path, resolve_cache_dir, resolve_capabilities_path,
    resolve_context_path, resolve_evidence_dir, resolve_presets_path, resolve_profile_key_path,
    resolve_profiles_path, resolve_projects_path, resolve_runbooks_path, resolve_state_path,
    resolve_store_info,
};
use crate::utils::when_matcher::{match_tags, matches_when};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone)]
pub struct WorkspaceService {
    logger: Logger,
    context_service: Arc<ContextService>,
    context_session: Option<Arc<ContextSessionService>>,
    project_resolver: Option<Arc<ProjectResolver>>,
    profile_service: Arc<ProfileService>,
    runbook_service: Arc<RunbookService>,
    capability_service: Arc<CapabilityService>,
    project_service: Arc<ProjectService>,
    alias_service: Arc<AliasService>,
    preset_service: Arc<PresetService>,
    state_service: Arc<StateService>,
}

impl WorkspaceService {
    pub fn new(
        logger: Logger,
        context_service: Arc<ContextService>,
        context_session: Option<Arc<ContextSessionService>>,
        project_resolver: Option<Arc<ProjectResolver>>,
        profile_service: Arc<ProfileService>,
        runbook_service: Arc<RunbookService>,
        capability_service: Arc<CapabilityService>,
        project_service: Arc<ProjectService>,
        alias_service: Arc<AliasService>,
        preset_service: Arc<PresetService>,
        state_service: Arc<StateService>,
    ) -> Self {
        Self {
            logger: logger.child("workspace"),
            context_service,
            context_session,
            project_resolver,
            profile_service,
            runbook_service,
            capability_service,
            project_service,
            alias_service,
            preset_service,
            state_service,
        }
    }

    async fn resolve_session(&self, args: &Value) -> Option<Value> {
        let session_service = self.context_session.as_ref()?;
        match session_service.resolve(args).await {
            Ok(value) => Some(value),
            Err(err) => {
                self.logger.warn(
                    "ContextSession resolve failed",
                    Some(&serde_json::json!({"error": err.message})),
                );
                None
            }
        }
    }

    async fn resolve_project_context(&self, args: &Value) -> Option<Value> {
        let resolver = self.project_resolver.as_ref()?;
        match resolver.resolve_context(args).await {
            Ok(Some(ctx)) => Some(ctx),
            Ok(None) => None,
            Err(err) => Some(serde_json::json!({"error": err.message})),
        }
    }

    fn build_store_items(&self) -> Vec<StoreItem> {
        vec![
            StoreItem::file("profiles", resolve_profiles_path(), true),
            StoreItem::file("projects", resolve_projects_path(), true),
            StoreItem::file("runbooks", resolve_runbooks_path(), false),
            StoreItem::file("capabilities", resolve_capabilities_path(), false),
            StoreItem::file("context", resolve_context_path(), true),
            StoreItem::file("aliases", resolve_aliases_path(), true),
            StoreItem::file("presets", resolve_presets_path(), true),
            StoreItem::file("audit", resolve_audit_path(), true),
            StoreItem::file("state", resolve_state_path(), true),
            StoreItem::file("key", resolve_profile_key_path(), true),
            StoreItem::dir("cache", resolve_cache_dir(), true),
            StoreItem::dir("evidence", resolve_evidence_dir(), true),
        ]
    }

    pub async fn store_status(&self, _args: &Value) -> Result<Value, ToolError> {
        let store_info = resolve_store_info();
        let mut files = serde_json::Map::new();
        for item in self.build_store_items() {
            files.insert(
                item.key.to_string(),
                serde_json::json!({
                    "exists": path_exists(&item.path),
                    "path": item.path.display().to_string(),
                    "kind": item.kind,
                    "sensitive": item.sensitive,
                }),
            );
        }
        Ok(serde_json::json!({
            "base_dir": store_info.get("base_dir").cloned().unwrap_or(Value::Null),
            "entry_dir": store_info.get("entry_dir").cloned().unwrap_or(Value::Null),
            "mode": store_info.get("mode").cloned().unwrap_or(Value::Null),
            "files": Value::Object(files),
        }))
    }

    async fn get_inventory(&self) -> Result<Value, ToolError> {
        let profiles = self.profile_service.list_profiles(None)?;
        let mut profile_counts: HashMap<String, usize> = HashMap::new();
        let mut profiles_total = 0usize;
        if let Some(list) = profiles.as_array() {
            profiles_total = list.len();
            for profile in list {
                if let Some(typ) = profile.get("type").and_then(|v| v.as_str()) {
                    *profile_counts.entry(typ.to_string()).or_insert(0) += 1;
                }
            }
        }

        let runbook_list = self
            .runbook_service
            .list_runbooks(&ListFilters::default())?;
        let runbooks = runbook_list
            .get("runbooks")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut runbook_sources: HashMap<String, usize> = HashMap::new();
        let mut runbook_tags: HashMap<String, usize> = HashMap::new();
        for runbook in runbooks.iter() {
            let source = runbook
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or("local");
            *runbook_sources.entry(source.to_string()).or_insert(0) += 1;
            if let Some(tags) = runbook.get("tags").and_then(|v| v.as_array()) {
                for tag in tags.iter().filter_map(|v| v.as_str()) {
                    *runbook_tags.entry(tag.to_string()).or_insert(0) += 1;
                }
            }
        }

        let capabilities = self.capability_service.list_capabilities()?;
        let mut capability_sources: HashMap<String, usize> = HashMap::new();
        let mut capability_total = 0usize;
        if let Some(list) = capabilities.as_array() {
            capability_total = list.len();
            for cap in list {
                let source = cap
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or("local");
                *capability_sources.entry(source.to_string()).or_insert(0) += 1;
            }
        }

        let projects = self
            .project_service
            .list_projects(&ListFilters::default())?;
        let projects_total = projects
            .get("meta")
            .and_then(|v| v.get("total"))
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .or_else(|| {
                projects
                    .get("projects")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.len())
            })
            .unwrap_or(0);

        let alias_stats = self.alias_service.get_stats();
        let preset_stats = self.preset_service.get_stats();
        let state_stats = self.state_service.get_stats();

        Ok(serde_json::json!({
            "profiles": {
                "total": profiles_total,
                "by_type": profile_counts,
            },
            "runbooks": {
                "total": runbooks.len(),
                "by_source": runbook_sources,
                "by_tag": runbook_tags,
            },
            "capabilities": {
                "total": capability_total,
                "by_source": capability_sources,
            },
            "projects": { "total": projects_total },
            "aliases": { "total": alias_stats.get("total").cloned().unwrap_or(Value::Number(0.into())) },
            "presets": { "total": preset_stats.get("total").cloned().unwrap_or(Value::Number(0.into())) },
            "state": {
                "session_keys": state_stats.get("session_keys").cloned().unwrap_or(Value::Number(0.into())),
                "persistent_keys": state_stats.get("persistent_keys").cloned().unwrap_or(Value::Number(0.into())),
            }
        }))
    }

    async fn suggest_capabilities(
        &self,
        context: &Value,
        limit: Option<usize>,
    ) -> Result<Vec<Value>, ToolError> {
        let capabilities = self.capability_service.list_capabilities()?;
        let mut suggestions = Vec::new();
        if let Some(list) = capabilities.as_array() {
            for cap in list {
                let when_clause = cap.get("when").unwrap_or(&Value::Null);
                if matches_when(when_clause, context) {
                    suggestions.push(serde_json::json!({
                        "name": cap.get("name").cloned().unwrap_or(Value::Null),
                        "intent": cap.get("intent").cloned().unwrap_or(Value::Null),
                        "description": cap.get("description").cloned().unwrap_or(Value::Null),
                        "tags": cap.get("tags").cloned().unwrap_or(Value::Array(vec![])),
                        "effects": cap.get("effects").cloned().unwrap_or(Value::Null),
                        "inputs": cap.get("inputs").cloned().unwrap_or(Value::Null),
                        "source": cap.get("source").cloned().unwrap_or(Value::String("local".to_string())),
                    }));
                }
            }
        }
        suggestions.sort_by_cached_key(name_of);
        if let Some(limit) = limit {
            suggestions.truncate(limit);
        }
        Ok(suggestions)
    }

    async fn suggest_runbooks(
        &self,
        context: &Value,
        limit: Option<usize>,
        include_untagged: bool,
    ) -> Result<Vec<Value>, ToolError> {
        let runbook_list = self
            .runbook_service
            .list_runbooks(&ListFilters::default())?;
        let runbooks = runbook_list
            .get("runbooks")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let context_tags = context
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let mut suggestions = Vec::new();
        for runbook in runbooks.iter() {
            let mut matched = false;
            let mut matched_tags: Vec<String> = Vec::new();

            if let Some(when_clause) = runbook.get("when") {
                if matches_when(when_clause, context) {
                    matched = true;
                }
            }

            if !matched {
                if let Some(tags) = runbook.get("tags").and_then(|v| v.as_array()) {
                    let runbook_tags = tags
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>();
                    matched_tags = match_tags(&runbook_tags, &context_tags);
                    matched = !matched_tags.is_empty();
                } else if include_untagged {
                    matched = true;
                }
            }

            if matched {
                let mut item = serde_json::Map::new();
                item.insert(
                    "name".to_string(),
                    runbook.get("name").cloned().unwrap_or(Value::Null),
                );
                item.insert(
                    "description".to_string(),
                    runbook.get("description").cloned().unwrap_or(Value::Null),
                );
                item.insert(
                    "tags".to_string(),
                    runbook.get("tags").cloned().unwrap_or(Value::Array(vec![])),
                );
                if let Some(inputs) = runbook.get("inputs") {
                    if !inputs.is_null() {
                        item.insert("inputs".to_string(), inputs.clone());
                    }
                }
                item.insert(
                    "source".to_string(),
                    runbook
                        .get("source")
                        .cloned()
                        .unwrap_or(Value::String("local".to_string())),
                );
                if !matched_tags.is_empty() {
                    item.insert(
                        "reason".to_string(),
                        serde_json::json!({"tags": matched_tags}),
                    );
                }
                suggestions.push(Value::Object(item));
            }
        }
        suggestions.sort_by_cached_key(name_of);
        if let Some(limit) = limit {
            suggestions.truncate(limit);
        }
        Ok(suggestions)
    }

    fn build_action_hints(
        &self,
        suggestions: &WorkspaceSuggestions,
        include_call: bool,
        context: &Value,
        project_context: Option<&Value>,
    ) -> Value {
        let mapping_context = serde_json::json!({
            "context": context,
            "project": project_context.and_then(|v| v.get("project")).cloned().unwrap_or(Value::Object(Default::default())),
            "target": project_context.and_then(|v| v.get("target")).cloned().unwrap_or(Value::Object(Default::default())),
        });

        let resolve_inputs = |inputs_meta: &Value| -> ResolvedInputs {
            let required = normalize_string_array(inputs_meta.get("required"));
            let defaults = inputs_meta
                .get("defaults")
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();
            let mapping = inputs_meta
                .get("map")
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();

            let mut resolved = defaults.clone();
            for (target_key, source_path) in mapping.iter() {
                if let Some(path) = source_path.as_str() {
                    if let Ok(value) =
                        get_path_value(&mapping_context, path, false, Some(Value::Null))
                    {
                        if !value.is_null() {
                            resolved.insert(target_key.clone(), value);
                        }
                    }
                }
            }

            let missing = required
                .iter()
                .filter(|key| {
                    resolved
                        .get(*key)
                        .map(|v| {
                            if let Some(text) = v.as_str() {
                                text.trim().is_empty()
                            } else {
                                v.is_null()
                            }
                        })
                        .unwrap_or(true)
                })
                .cloned()
                .collect::<Vec<_>>();

            ResolvedInputs {
                required,
                defaults,
                mapping,
                resolved,
                missing,
            }
        };

        let mut intent_actions = Vec::new();
        for cap in suggestions.capabilities.iter() {
            let inputs_meta = cap.get("inputs").cloned().unwrap_or(Value::Null);
            let resolved = resolve_inputs(&inputs_meta);
            let template = build_input_template(
                &resolved.required,
                &merge_maps(&resolved.defaults, &resolved.resolved),
            );
            let requires_apply = cap
                .get("effects")
                .and_then(|v| v.get("requires_apply"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mut call_args = serde_json::Map::new();
            call_args.insert("action".to_string(), Value::String("run".to_string()));
            call_args.insert(
                "intent_type".to_string(),
                cap.get("intent").cloned().unwrap_or(Value::Null),
            );
            call_args.insert("inputs".to_string(), Value::Object(template.clone()));
            if requires_apply {
                call_args.insert("apply".to_string(), Value::Bool(true));
            }
            let call = if include_call {
                Some(serde_json::json!({
                    "tool": "mcp_workspace",
                    "args": Value::Object(call_args),
                }))
            } else {
                None
            };
            intent_actions.push(serde_json::json!({
                "kind": "intent",
                "name": cap.get("name").cloned().unwrap_or(Value::Null),
                "intent": cap.get("intent").cloned().unwrap_or(Value::Null),
                "description": cap.get("description").cloned().unwrap_or(Value::Null),
                "tags": cap.get("tags").cloned().unwrap_or(Value::Array(vec![])),
                "effects": cap.get("effects").cloned().unwrap_or(Value::Null),
                "inputs": resolved.as_json(),
                "call": call,
            }));
        }

        let mut runbook_actions = Vec::new();
        for runbook in suggestions.runbooks.iter() {
            let inputs_meta = serde_json::json!({
                "required": runbook.get("inputs").cloned().unwrap_or(Value::Array(vec![]))
            });
            let resolved = resolve_inputs(&inputs_meta);
            let template = build_input_template(&resolved.required, &resolved.resolved);
            let call = if include_call {
                Some(serde_json::json!({
                    "tool": "mcp_workspace",
                    "args": {
                        "action": "run",
                        "name": runbook.get("name").cloned().unwrap_or(Value::Null),
                        "input": Value::Object(template.clone()),
                    }
                }))
            } else {
                None
            };
            let mut item = serde_json::Map::new();
            item.insert("kind".to_string(), Value::String("runbook".to_string()));
            item.insert(
                "name".to_string(),
                runbook.get("name").cloned().unwrap_or(Value::Null),
            );
            item.insert(
                "description".to_string(),
                runbook.get("description").cloned().unwrap_or(Value::Null),
            );
            item.insert(
                "tags".to_string(),
                runbook.get("tags").cloned().unwrap_or(Value::Array(vec![])),
            );
            item.insert(
                "inputs".to_string(),
                serde_json::json!({
                    "required": resolved.required,
                    "resolved": resolved.resolved,
                    "missing": resolved.missing,
                }),
            );
            if let Some(reason) = runbook.get("reason") {
                if !reason.is_null() {
                    item.insert("reason".to_string(), reason.clone());
                }
            }
            if let Some(call) = call {
                item.insert("call".to_string(), call);
            }
            runbook_actions.push(Value::Object(item));
        }

        serde_json::json!({
            "intents": intent_actions,
            "runbooks": runbook_actions,
        })
    }

    pub async fn summarize(&self, args: &Value) -> Result<Value, ToolError> {
        let session = self.resolve_session(args).await;
        let context_result = if let Some(session) = session.as_ref() {
            let context = session
                .get("effective_context")
                .or_else(|| session.get("context"))
                .cloned()
                .unwrap_or(Value::Object(Default::default()));
            serde_json::json!({"context": context, "diagnostics": session.get("diagnostics").cloned().unwrap_or(Value::Null), "bindings": session.get("bindings").cloned().unwrap_or(Value::Null), "project_context": session.get("project_context").cloned().unwrap_or(Value::Null)})
        } else {
            self.context_service.get_context(args).await?
        };

        let context = context_result
            .get("context")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let project_context = if let Some(session) = session.as_ref() {
            session.get("project_context").cloned().and_then(|v| {
                if v.is_null() {
                    None
                } else {
                    Some(v)
                }
            })
        } else {
            self.resolve_project_context(args).await
        };
        let store = self.store_status(args).await?;
        let inventory = self.get_inventory().await?;

        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        let include_untagged = args
            .get("include_untagged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let include_call = args
            .get("include_call")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let suggestions = WorkspaceSuggestions {
            capabilities: self.suggest_capabilities(&context, limit).await?,
            runbooks: self
                .suggest_runbooks(&context, limit, include_untagged)
                .await?,
        };
        let actions = self.build_action_hints(
            &suggestions,
            include_call,
            &context,
            project_context.as_ref(),
        );

        let view = serde_json::json!({
            "format": args.get("format").cloned().unwrap_or(Value::String("full".to_string())),
            "limit": args.get("limit").cloned().unwrap_or(Value::Null),
            "include_call": include_call,
        });

        let signals_true = context
            .get("signals")
            .and_then(|v| v.as_object())
            .map(|signals| {
                let mut out = signals
                    .iter()
                    .filter_map(|(key, value)| value.as_bool().filter(|b| *b).map(|_| key.clone()))
                    .collect::<Vec<_>>();
                out.sort();
                out
            })
            .unwrap_or_default();
        let evidence_files = context
            .get("files")
            .and_then(|v| v.as_object())
            .map(|files| {
                let mut out = files
                    .iter()
                    .filter_map(|(key, value)| value.as_bool().filter(|b| *b).map(|_| key.clone()))
                    .collect::<Vec<_>>();
                out.sort();
                out.truncate(80);
                out
            })
            .unwrap_or_default();

        let project_context_error = project_context
            .as_ref()
            .and_then(|v| v.get("error"))
            .cloned();

        let base_workspace = serde_json::json!({
            "context": {
                "key": context.get("key").cloned().unwrap_or(Value::Null),
                "root": context.get("root").cloned().unwrap_or(Value::Null),
                "tags": context.get("tags").cloned().unwrap_or(Value::Array(vec![])),
                "signals_true": signals_true,
                "evidence_files": evidence_files,
                "git_root": context.get("git").and_then(|v| v.get("root")).cloned().unwrap_or(Value::Null),
                "project_name": context.get("project_name").cloned().unwrap_or(Value::Null),
                "target_name": context.get("target_name").cloned().unwrap_or(Value::Null),
                "updated_at": context.get("updated_at").cloned().unwrap_or(Value::Null),
            },
            "project": project_context.as_ref().and_then(|ctx| {
                if ctx.get("error").is_some() { return None; }
                Some(serde_json::json!({
                    "name": ctx.get("projectName").cloned().unwrap_or(Value::Null),
                    "target": ctx.get("targetName").cloned().unwrap_or(Value::Null),
                    "description": ctx.get("project").and_then(|v| v.get("description")).cloned().unwrap_or(Value::Null),
                    "repo_root": ctx.get("project").and_then(|v| v.get("repo_root")).cloned().unwrap_or(Value::Null),
                    "target_info": ctx.get("target").cloned().unwrap_or(Value::Null),
                }))
            }),
            "project_error": project_context_error,
            "diagnostics": session.as_ref().and_then(|s| s.get("diagnostics")).cloned().unwrap_or(Value::Null),
            "bindings": session.as_ref().and_then(|s| s.get("bindings")).cloned().unwrap_or(Value::Null),
            "suggestions": suggestions.as_json(),
            "actions": actions,
            "view": view,
        });

        let format = args
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("full");
        if format == "actions" {
            return Ok(serde_json::json!({
                "success": true,
                "context": base_workspace.get("context").cloned().unwrap_or(Value::Null),
                "project": base_workspace.get("project").cloned().unwrap_or(Value::Null),
                "diagnostics": base_workspace.get("diagnostics").cloned().unwrap_or(Value::Null),
                "bindings": base_workspace.get("bindings").cloned().unwrap_or(Value::Null),
                "actions": base_workspace.get("actions").cloned().unwrap_or(Value::Null),
                "view": view,
            }));
        }

        if format == "compact" {
            return Ok(serde_json::json!({
                "success": true,
                "workspace": base_workspace,
            }));
        }

        Ok(serde_json::json!({
            "success": true,
            "workspace": {
                "context": base_workspace.get("context").cloned().unwrap_or(Value::Null),
                "project": base_workspace.get("project").cloned().unwrap_or(Value::Null),
                "project_error": base_workspace.get("project_error").cloned().unwrap_or(Value::Null),
                "diagnostics": base_workspace.get("diagnostics").cloned().unwrap_or(Value::Null),
                "bindings": base_workspace.get("bindings").cloned().unwrap_or(Value::Null),
                "suggestions": base_workspace.get("suggestions").cloned().unwrap_or(Value::Null),
                "actions": base_workspace.get("actions").cloned().unwrap_or(Value::Null),
                "view": view,
                "store": store,
                "inventory": inventory,
            }
        }))
    }

    pub async fn suggest(&self, args: &Value) -> Result<Value, ToolError> {
        let session = self.resolve_session(args).await;
        let context_result = if let Some(session) = session.as_ref() {
            let context = session
                .get("effective_context")
                .or_else(|| session.get("context"))
                .cloned()
                .unwrap_or(Value::Object(Default::default()));
            serde_json::json!({"context": context})
        } else {
            self.context_service.get_context(args).await?
        };
        let context = context_result
            .get("context")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));

        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        let include_untagged = args
            .get("include_untagged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let include_call = args
            .get("include_call")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let suggestions = WorkspaceSuggestions {
            capabilities: self.suggest_capabilities(&context, limit).await?,
            runbooks: self
                .suggest_runbooks(&context, limit, include_untagged)
                .await?,
        };
        let actions = self.build_action_hints(&suggestions, include_call, &context, None);

        let view = serde_json::json!({
            "format": args.get("format").cloned().unwrap_or(Value::String("suggest".to_string())),
            "limit": args.get("limit").cloned().unwrap_or(Value::Null),
            "include_call": include_call,
        });

        let format = args
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("suggest");
        if format == "actions" {
            return Ok(serde_json::json!({
                "success": true,
                "context": {
                    "key": context.get("key").cloned().unwrap_or(Value::Null),
                    "root": context.get("root").cloned().unwrap_or(Value::Null),
                    "tags": context.get("tags").cloned().unwrap_or(Value::Array(vec![])),
                },
                "diagnostics": session.as_ref().and_then(|s| s.get("diagnostics")).cloned().unwrap_or(Value::Null),
                "bindings": session.as_ref().and_then(|s| s.get("bindings")).cloned().unwrap_or(Value::Null),
                "actions": actions,
                "view": view,
            }));
        }

        Ok(serde_json::json!({
            "success": true,
            "context": {
                "key": context.get("key").cloned().unwrap_or(Value::Null),
                "root": context.get("root").cloned().unwrap_or(Value::Null),
                "tags": context.get("tags").cloned().unwrap_or(Value::Array(vec![])),
            },
            "diagnostics": session.as_ref().and_then(|s| s.get("diagnostics")).cloned().unwrap_or(Value::Null),
            "bindings": session.as_ref().and_then(|s| s.get("bindings")).cloned().unwrap_or(Value::Null),
            "suggestions": suggestions.as_json(),
            "actions": actions,
            "view": view,
        }))
    }

    pub async fn diagnose(&self, args: &Value) -> Result<Value, ToolError> {
        let store = self.store_status(args).await?;
        let context_result = self.context_service.get_context(args).await?;
        let context = context_result
            .get("context")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let project_context = self.resolve_project_context(args).await;
        let inventory = self.get_inventory().await?;

        let mut warnings = Vec::new();
        let mut hints = Vec::new();

        let store_base = store.get("base_dir").and_then(|v| v.as_str());
        if let Some(base) = store_base {
            let git_root = find_git_root(Path::new(base));
            let mode = store.get("mode").and_then(|v| v.as_str()).unwrap_or("");
            if git_root.is_some() && mode != "custom" {
                warnings.push(serde_json::json!({
                    "code": "store_inside_repo",
                    "message": format!("Хранилище расположено внутри git-репозитория: {}", base),
                    "action": { "tool": "mcp_workspace", "args": { "action": "store_status" } },
                }));
            }
        }

        if let Some(ctx) = project_context.as_ref() {
            if ctx.get("error").is_none() {
                if let Some(target) = ctx.get("target").and_then(|v| v.as_object()) {
                    let mut missing = Vec::new();
                    for (label, value) in [
                        ("ssh_profile", target.get("ssh_profile")),
                        ("env_profile", target.get("env_profile")),
                        ("postgres_profile", target.get("postgres_profile")),
                        ("api_profile", target.get("api_profile")),
                        ("vault_profile", target.get("vault_profile")),
                    ] {
                        if let Some(value) = value.and_then(|v| v.as_str()) {
                            if !value.trim().is_empty() && !self.profile_service.has_profile(value)
                            {
                                missing.push(label);
                            }
                        }
                    }
                    if !missing.is_empty() {
                        warnings.push(serde_json::json!({
                            "code": "missing_profiles",
                            "message": format!("Не найдены профили для target: {}", missing.join(", ")),
                        }));
                    }
                }
            }
        }

        if inventory
            .get("runbooks")
            .and_then(|v| v.get("total"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            == 0
        {
            hints.push(serde_json::json!({
                "code": "no_runbooks",
                "message": "Нет доступных runbook-ов. Добавьте через mcp_runbook."
            }));
        }

        if inventory
            .get("capabilities")
            .and_then(|v| v.get("total"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            == 0
        {
            hints.push(serde_json::json!({
                "code": "no_capabilities",
                "message": "Нет доступных capability. Проверьте capabilities.json."
            }));
        }

        Ok(serde_json::json!({
            "success": true,
            "diagnostics": { "warnings": warnings, "hints": hints },
            "context": {
                "key": context.get("key").cloned().unwrap_or(Value::Null),
                "root": context.get("root").cloned().unwrap_or(Value::Null),
                "tags": context.get("tags").cloned().unwrap_or(Value::Array(vec![])),
            },
            "store": store,
            "inventory": inventory,
            "project": project_context.as_ref().and_then(|ctx| {
                if ctx.get("error").is_some() { return None; }
                Some(serde_json::json!({
                    "name": ctx.get("projectName").cloned().unwrap_or(Value::Null),
                    "target": ctx.get("targetName").cloned().unwrap_or(Value::Null),
                }))
            }),
            "project_error": project_context.as_ref().and_then(|v| v.get("error")).cloned().unwrap_or(Value::Null),
        }))
    }

    pub async fn stats(&self, args: &Value) -> Result<Value, ToolError> {
        Ok(serde_json::json!({
            "success": true,
            "store": self.store_status(args).await?,
            "inventory": self.get_inventory().await?,
        }))
    }
}

struct StoreItem {
    key: &'static str,
    path: PathBuf,
    kind: &'static str,
    sensitive: bool,
}

impl StoreItem {
    fn file(key: &'static str, path: PathBuf, sensitive: bool) -> Self {
        Self {
            key,
            path,
            kind: "file",
            sensitive,
        }
    }

    fn dir(key: &'static str, path: PathBuf, sensitive: bool) -> Self {
        Self {
            key,
            path,
            kind: "dir",
            sensitive,
        }
    }
}

struct WorkspaceSuggestions {
    capabilities: Vec<Value>,
    runbooks: Vec<Value>,
}

impl WorkspaceSuggestions {
    fn as_json(&self) -> Value {
        serde_json::json!({
            "capabilities": self.capabilities,
            "runbooks": self.runbooks,
        })
    }
}

struct ResolvedInputs {
    required: Vec<String>,
    defaults: serde_json::Map<String, Value>,
    mapping: serde_json::Map<String, Value>,
    resolved: serde_json::Map<String, Value>,
    missing: Vec<String>,
}

impl ResolvedInputs {
    fn as_json(&self) -> Value {
        serde_json::json!({
            "required": self.required,
            "defaults": self.defaults,
            "map": self.mapping,
            "resolved": self.resolved,
            "missing": self.missing,
        })
    }
}

fn name_of(value: &Value) -> String {
    value
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn normalize_string_array(value: Option<&Value>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .collect()
}

fn build_input_template(
    required: &[String],
    defaults: &serde_json::Map<String, Value>,
) -> serde_json::Map<String, Value> {
    let mut template = serde_json::Map::new();
    for key in required.iter() {
        if let Some(value) = defaults.get(key) {
            template.insert(key.clone(), value.clone());
        } else {
            template.insert(key.clone(), Value::String(format!("<{}>", key)));
        }
    }
    for (key, value) in defaults.iter() {
        template.entry(key.clone()).or_insert_with(|| value.clone());
    }
    template
}

fn merge_maps(
    left: &serde_json::Map<String, Value>,
    right: &serde_json::Map<String, Value>,
) -> serde_json::Map<String, Value> {
    let mut out: BTreeMap<String, Value> = BTreeMap::new();
    for (key, value) in left.iter() {
        out.insert(key.clone(), value.clone());
    }
    for (key, value) in right.iter() {
        out.insert(key.clone(), value.clone());
    }
    out.into_iter().collect()
}

fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    for _ in 0..25 {
        if current.join(".git").exists() {
            return Some(current);
        }
        let parent = current.parent()?.to_path_buf();
        if parent == current {
            break;
        }
        current = parent;
    }
    None
}
