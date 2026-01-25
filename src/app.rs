use crate::errors::ToolError;
use crate::managers;
use crate::mcp::catalog::tool_catalog;
use crate::services::alias::AliasService;
use crate::services::audit::AuditService;
use crate::services::cache::CacheService;
use crate::services::capability::CapabilityService;
use crate::services::context::ContextService;
use crate::services::context_session::ContextSessionService;
use crate::services::evidence::EvidenceService;
use crate::services::job::JobService;
use crate::services::logger::Logger;
use crate::services::policy::PolicyService;
use crate::services::preset::PresetService;
use crate::services::profile::ProfileService;
use crate::services::project::ProjectService;
use crate::services::project_resolver::ProjectResolver;
use crate::services::runbook::RunbookService;
use crate::services::secret_ref::SecretRefResolver;
use crate::services::security::Security;
use crate::services::state::StateService;
use crate::services::tool_executor::{ToolExecutor, ToolHandler};
use crate::services::validation::Validation;
use crate::services::vault_client::VaultClient;
use crate::services::workspace::WorkspaceService;
use std::collections::HashMap;
use std::sync::Arc;

pub struct App {
    pub logger: Logger,
    pub tool_executor: Arc<ToolExecutor>,
    pub alias_service: Option<Arc<AliasService>>,
    pub runbook_manager: Arc<managers::runbook::RunbookManager>,
}

impl App {
    fn validate_tool_wiring(
        handlers: &HashMap<String, Arc<dyn ToolHandler>>,
        alias_map: &HashMap<String, String>,
    ) -> Result<(), ToolError> {
        let builtins = ["help", "legend"];
        let mut missing = Vec::new();
        for tool in tool_catalog().iter() {
            if builtins.contains(&tool.name.as_str()) {
                continue;
            }
            if handlers.contains_key(&tool.name) {
                continue;
            }
            if alias_map.contains_key(&tool.name) {
                continue;
            }
            missing.push(tool.name.clone());
        }
        if missing.is_empty() {
            return Ok(());
        }
        missing.sort();
        Err(ToolError::internal("Tool wiring is incomplete").with_hint(
            "This is a server wiring bug: every tool in tool_catalog.json must have a handler or an alias_map entry."
                .to_string(),
        )
        .with_details(serde_json::json!({ "missing_tools": missing })))
    }

    pub fn initialize() -> Result<Self, ToolError> {
        let logger = Logger::new("infra");
        let validation = Validation::new();

        let security = Arc::new(Security::new()?);
        let state_service = Arc::new(StateService::new()?);
        let profile_service = Arc::new(ProfileService::new(security.clone())?);
        let project_service = Arc::new(ProjectService::new()?);
        let project_resolver = Arc::new(ProjectResolver::new(
            validation.clone(),
            project_service.clone(),
            Some(state_service.clone()),
        ));
        let context_service = Arc::new(ContextService::new()?);
        let runbook_service = Arc::new(RunbookService::new()?);
        let capability_service = Arc::new(CapabilityService::new(security.clone())?);
        let alias_service = Arc::new(AliasService::new()?);
        let preset_service = Arc::new(PresetService::new()?);
        let audit_service = Arc::new(AuditService::new(logger.clone()));
        let cache_service = Arc::new(CacheService::new(logger.clone()));
        let job_service = Arc::new(JobService::new(logger.clone())?);
        let evidence_service = Arc::new(EvidenceService::new(logger.clone(), (*security).clone()));
        let vault_client = Arc::new(VaultClient::new(
            logger.clone(),
            validation.clone(),
            profile_service.clone(),
        ));
        let policy_service = Arc::new(PolicyService::new(
            logger.clone(),
            Some(state_service.clone()),
        ));
        let context_session = Arc::new(ContextSessionService::new(
            logger.clone(),
            context_service.clone(),
            Some(project_resolver.clone()),
            Some(profile_service.clone()),
        ));
        let workspace_service = Arc::new(WorkspaceService::new(
            logger.clone(),
            context_service.clone(),
            Some(context_session.clone()),
            Some(project_resolver.clone()),
            profile_service.clone(),
            runbook_service.clone(),
            capability_service.clone(),
            project_service.clone(),
            alias_service.clone(),
            preset_service.clone(),
            state_service.clone(),
        ));
        let secret_ref_resolver = Arc::new(SecretRefResolver::new(
            logger.clone(),
            validation.clone(),
            Some(profile_service.clone()),
            Some(vault_client.clone()),
            Some(project_resolver.clone()),
        ));

        let alias_manager = Arc::new(managers::alias::AliasManager::new(
            logger.clone(),
            alias_service.clone(),
        ));
        let preset_manager = Arc::new(managers::preset::PresetManager::new(
            logger.clone(),
            preset_service.clone(),
        ));
        let state_manager = Arc::new(managers::state::StateManager::new(
            logger.clone(),
            state_service.clone(),
        ));
        let audit_manager = Arc::new(managers::audit::AuditManager::new(
            logger.clone(),
            audit_service.clone(),
        ));
        let artifacts_manager = Arc::new(managers::artifacts::ArtifactManager::new(logger.clone()));
        let context_manager = Arc::new(managers::context::ContextManager::new(
            logger.clone(),
            context_service.clone(),
        ));
        let project_manager = Arc::new(managers::project::ProjectManager::new(
            logger.clone(),
            validation.clone(),
            project_service.clone(),
            state_service.clone(),
        ));
        let capability_manager = Arc::new(managers::capability::CapabilityManager::new(
            logger.clone(),
            validation.clone(),
            capability_service.clone(),
            Some(context_service.clone()),
        ));
        let evidence_manager = Arc::new(managers::evidence::EvidenceManager::new(
            logger.clone(),
            evidence_service.clone(),
        ));
        let ssh_manager = Arc::new(managers::ssh::SshManager::new(
            logger.clone(),
            security.clone(),
            validation.clone(),
            profile_service.clone(),
            Some(project_resolver.clone()),
            Some(secret_ref_resolver.clone()),
            Some(job_service.clone()),
        ));
        let env_manager = Arc::new(managers::env::EnvManager::new(
            logger.clone(),
            validation.clone(),
            profile_service.clone(),
            ssh_manager.clone(),
            Some(project_resolver.clone()),
            Some(secret_ref_resolver.clone()),
        ));
        let vault_manager = Arc::new(managers::vault::VaultManager::new(
            logger.clone(),
            validation.clone(),
            profile_service.clone(),
            Some(vault_client.clone()),
        ));
        let api_manager = Arc::new(managers::api::ApiManager::new(
            logger.clone(),
            validation.clone(),
            profile_service.clone(),
            Some(cache_service.clone()),
            Some(project_resolver.clone()),
            Some(secret_ref_resolver.clone()),
        ));
        let postgres_manager = Arc::new(managers::postgres::PostgresManager::new(
            logger.clone(),
            validation.clone(),
            profile_service.clone(),
            Some(project_resolver.clone()),
            Some(secret_ref_resolver.clone()),
        ));
        let local_manager = Arc::new(managers::local::LocalManager::new(
            logger.clone(),
            validation.clone(),
            None,
        ));
        let repo_manager = Arc::new(managers::repo::RepoManager::new(logger.clone()));
        let pipeline_manager = Arc::new(managers::pipeline::PipelineManager::new(
            logger.clone(),
            validation.clone(),
            api_manager.clone(),
            ssh_manager.clone(),
            postgres_manager.clone(),
            Some(cache_service.clone()),
            Some(audit_service.clone()),
            Some(project_resolver.clone()),
        ));
        let intent_manager = Arc::new(managers::intent::IntentManager::new(
            logger.clone(),
            security.clone(),
            validation.clone(),
            capability_service.clone(),
            runbook_service.clone(),
            evidence_service.clone(),
            state_service.clone(),
            Some(project_resolver.clone()),
            Some(context_service.clone()),
            Some(policy_service.clone()),
        ));
        let job_manager = Arc::new(managers::jobs::JobManager::new(
            logger.clone(),
            validation.clone(),
            job_service.clone(),
            Some(ssh_manager.clone()),
        ));
        let runbook_manager = Arc::new(managers::runbook::RunbookManager::new(
            logger.clone(),
            runbook_service.clone(),
            state_service.clone(),
        ));
        let workspace_manager = Arc::new(managers::workspace::WorkspaceManager::new(
            logger.clone(),
            validation.clone(),
            workspace_service.clone(),
            runbook_manager.clone(),
            Some(intent_manager.clone()),
            Some(ssh_manager.clone()),
        ));

        let mut handlers: HashMap<String, Arc<dyn ToolHandler>> = HashMap::new();
        handlers.insert("mcp_alias".to_string(), alias_manager);
        handlers.insert("mcp_preset".to_string(), preset_manager);
        handlers.insert("mcp_state".to_string(), state_manager);
        handlers.insert("mcp_audit".to_string(), audit_manager);
        handlers.insert("mcp_artifacts".to_string(), artifacts_manager);
        handlers.insert("mcp_context".to_string(), context_manager);
        handlers.insert("mcp_project".to_string(), project_manager);
        handlers.insert("mcp_capability".to_string(), capability_manager);
        handlers.insert("mcp_evidence".to_string(), evidence_manager);
        handlers.insert("mcp_workspace".to_string(), workspace_manager);
        handlers.insert("mcp_runbook".to_string(), runbook_manager.clone());
        handlers.insert("mcp_env".to_string(), env_manager);
        handlers.insert("mcp_vault".to_string(), vault_manager);
        handlers.insert("mcp_ssh_manager".to_string(), ssh_manager);
        handlers.insert("mcp_api_client".to_string(), api_manager);
        handlers.insert("mcp_psql_manager".to_string(), postgres_manager);
        handlers.insert("mcp_local".to_string(), local_manager);
        handlers.insert("mcp_repo".to_string(), repo_manager);
        handlers.insert("mcp_pipeline".to_string(), pipeline_manager);
        handlers.insert("mcp_intent".to_string(), intent_manager.clone());
        handlers.insert("mcp_jobs".to_string(), job_manager);

        let alias_map = crate::mcp::aliases::builtin_tool_alias_map_owned();

        Self::validate_tool_wiring(&handlers, &alias_map)?;

        let tool_executor = Arc::new(ToolExecutor::new(
            logger.clone(),
            state_service.clone(),
            Some(alias_service.clone()),
            Some(preset_service.clone()),
            Some(audit_service.clone()),
            handlers,
            alias_map,
        ));

        intent_manager.set_tool_executor(tool_executor.clone());
        runbook_manager.set_tool_executor(tool_executor.clone());

        Ok(Self {
            logger,
            tool_executor,
            alias_service: Some(alias_service),
            runbook_manager,
        })
    }
}
