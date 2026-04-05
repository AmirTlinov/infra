use crate::errors::ToolError;
use crate::managers;
use crate::services::alias::AliasService;
use crate::services::audit::AuditService;
use crate::services::cache::CacheService;
use crate::services::capability::CapabilityService;
use crate::services::context::ContextService;
use crate::services::context_session::ContextSessionService;
use crate::services::description::DescriptionService;
use crate::services::evidence::EvidenceService;
use crate::services::job::JobService;
use crate::services::logger::Logger;
use crate::services::operation::OperationService;
use crate::services::policy::PolicyService;
use crate::services::preset::PresetService;
use crate::services::profile::ProfileService;
use crate::services::project::ProjectService;
use crate::services::project_resolver::ProjectResolver;
use crate::services::runbook::RunbookService;
use crate::services::secret_ref::SecretRefResolver;
use crate::services::security::Security;
use crate::services::state::{new_session_state, SessionState, StateService};
use crate::services::tool_executor::{ToolExecutor, ToolHandler};
use crate::services::validation::Validation;
use crate::services::vault_client::VaultClient;
use crate::services::workspace::WorkspaceService;
use crate::tooling::catalog::tool_contract_catalog;
use crate::tooling::names::builtin_tool_alias_map_owned;
use std::collections::HashMap;
use std::sync::Arc;

pub struct App {
    pub logger: Logger,
    pub tool_executor: Arc<ToolExecutor>,
    pub alias_service: Option<Arc<AliasService>>,
    pub capability_service: Arc<CapabilityService>,
    pub operation_service: Arc<OperationService>,
    pub runbook_service: Arc<RunbookService>,
    pub workspace_service: Arc<WorkspaceService>,
    pub runbook_manager: Arc<managers::runbook::RunbookManager>,
    pub state_service: Arc<StateService>,
    pub job_service: Arc<JobService>,
    pub project_manager: Arc<managers::project::ProjectManager>,
    pub target_manager: Arc<managers::target::TargetManager>,
    pub profile_manager: Arc<managers::profile::ProfileManager>,
    pub capability_manager: Arc<managers::capability::CapabilityManager>,
    pub policy_manager: Arc<managers::policy::PolicyManager>,
    pub operation_manager: Arc<managers::operation::OperationManager>,
    pub receipt_manager: Arc<managers::receipt::ReceiptManager>,
    pub job_manager: Arc<managers::jobs::JobManager>,
}

impl App {
    fn validate_tool_wiring(
        handlers: &HashMap<String, Arc<dyn ToolHandler>>,
        alias_map: &HashMap<String, String>,
    ) -> Result<(), ToolError> {
        let mut missing = Vec::new();
        for tool in tool_contract_catalog().iter() {
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
            "This is an app wiring bug: every canonical tool contract must have a handler or an alias entry."
                .to_string(),
        )
        .with_details(serde_json::json!({ "missing_tools": missing })))
    }

    pub fn initialize() -> Result<Self, ToolError> {
        Self::initialize_with_session(new_session_state())
    }

    pub fn initialize_with_session(session_state: SessionState) -> Result<Self, ToolError> {
        let logger = Logger::new("infra");
        let validation = Validation::new();

        let security = Arc::new(Security::new()?);
        let state_service = Arc::new(StateService::new_with_session(session_state)?);
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
        let operation_service = Arc::new(OperationService::new()?);
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
        let profile_manager = Arc::new(managers::profile::ProfileManager::new(
            logger.clone(),
            profile_service.clone(),
        ));
        let project_manager = Arc::new(managers::project::ProjectManager::new(
            logger.clone(),
            validation.clone(),
            project_service.clone(),
            state_service.clone(),
        ));
        let target_manager = Arc::new(managers::target::TargetManager::new(
            logger.clone(),
            validation.clone(),
            project_service.clone(),
            state_service.clone(),
            Some(profile_service.clone()),
            Some(policy_service.clone()),
        ));
        let policy_manager = Arc::new(managers::policy::PolicyManager::new(
            logger.clone(),
            policy_service.clone(),
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
        let operation_manager = Arc::new(managers::operation::OperationManager::new(
            logger.clone(),
            validation.clone(),
            capability_service.clone(),
            runbook_service.clone(),
            Some(context_service.clone()),
            intent_manager.clone(),
            operation_service.clone(),
            job_service.clone(),
        ));
        let receipt_manager = Arc::new(managers::receipt::ReceiptManager::new(
            logger.clone(),
            validation.clone(),
            operation_service.clone(),
            job_service.clone(),
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
        handlers.insert("alias".to_string(), alias_manager);
        handlers.insert("preset".to_string(), preset_manager);
        handlers.insert("state".to_string(), state_manager);
        handlers.insert("audit".to_string(), audit_manager);
        handlers.insert("artifacts".to_string(), artifacts_manager);
        handlers.insert("context".to_string(), context_manager);
        handlers.insert("profile".to_string(), profile_manager.clone());
        handlers.insert("project".to_string(), project_manager.clone());
        handlers.insert("target".to_string(), target_manager.clone());
        handlers.insert("policy".to_string(), policy_manager.clone());
        handlers.insert("capability".to_string(), capability_manager.clone());
        handlers.insert("evidence".to_string(), evidence_manager);
        handlers.insert("workspace".to_string(), workspace_manager);
        handlers.insert("runbook".to_string(), runbook_manager.clone());
        handlers.insert("env".to_string(), env_manager);
        handlers.insert("vault".to_string(), vault_manager);
        handlers.insert("ssh".to_string(), ssh_manager);
        handlers.insert("api".to_string(), api_manager);
        handlers.insert("sql".to_string(), postgres_manager);
        handlers.insert("local".to_string(), local_manager);
        handlers.insert("repo".to_string(), repo_manager);
        handlers.insert("pipeline".to_string(), pipeline_manager);
        handlers.insert("intent".to_string(), intent_manager.clone());
        handlers.insert("job".to_string(), job_manager.clone());
        handlers.insert("operation".to_string(), operation_manager.clone());
        handlers.insert("receipt".to_string(), receipt_manager.clone());

        let alias_map = builtin_tool_alias_map_owned();

        Self::validate_tool_wiring(&handlers, &alias_map)?;

        let tool_executor = Arc::new(ToolExecutor::new(
            logger.clone(),
            state_service.clone(),
            Some(alias_service.clone()),
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
            capability_service,
            operation_service,
            runbook_service,
            workspace_service,
            runbook_manager,
            state_service,
            job_service,
            project_manager,
            target_manager,
            profile_manager,
            capability_manager,
            policy_manager,
            operation_manager,
            receipt_manager,
            job_manager,
        })
    }

    pub fn description_snapshot(&self) -> Result<serde_json::Value, ToolError> {
        DescriptionService::snapshot(
            self.capability_service.as_ref(),
            self.runbook_service.as_ref(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[derive(Clone)]
    struct NoopHandler;

    #[async_trait::async_trait]
    impl ToolHandler for NoopHandler {
        async fn handle(&self, _args: Value) -> Result<Value, ToolError> {
            Ok(serde_json::json!({ "success": true }))
        }
    }

    #[test]
    fn validate_tool_wiring_requires_canonical_handlers() {
        let missing_tool = "receipt";
        assert!(tool_contract_catalog()
            .iter()
            .any(|tool| tool.name == missing_tool));

        let mut handlers: HashMap<String, Arc<dyn ToolHandler>> = HashMap::new();
        for tool in tool_contract_catalog().iter() {
            if tool.name == missing_tool {
                continue;
            }
            handlers.insert(tool.name.clone(), Arc::new(NoopHandler));
        }

        let err = App::validate_tool_wiring(&handlers, &HashMap::new())
            .expect_err("canonical tool contract should be required");
        assert_eq!(err.message, "Tool wiring is incomplete");
        assert_eq!(
            err.details
                .as_ref()
                .and_then(|value| value.get("missing_tools")),
            Some(&serde_json::json!([missing_tool]))
        );
    }
}
