//! Driver-owned optional capabilities injected into the environment host.

use std::{path::Path, sync::Arc, time::Duration};

use omp_cache::telemetry_cache::TelemetryIndex;
use omp_core::{EnvPath, Str};
use omp_env::EnvClient;
use omp_envd::github_url::GithubCredentialBridge;
use omp_inference::auth::command::CommandCredentialExecutor;

use crate::auth_backend::EnvCommandCredentialExecutor;

/// Inference service binding retained by compositions that enable search.
#[derive(Default)]
pub struct InferenceBridge;

/// Session goal-control binding.
#[derive(Clone, Default)]
pub struct AgentGoalControl;

/// Runs `!command` credential sources inside the project Environment.
///
/// Bounds mirror pi `model-config-values.ts` (`execSync` with a 10 s timeout
/// and Node's 1 MiB default `maxBuffer`).
#[derive(Clone, Copy, Debug, Default)]
pub struct CommandCredentials;

impl CommandCredentials {
	const MAX_STDOUT: usize = 1 << 20;
	const TIMEOUT: Duration = Duration::from_secs(10);
}

impl omp_envd::CommandCredentialExecutorFactory for CommandCredentials {
	fn make(&self, client: EnvClient, cwd: &Path) -> Arc<dyn CommandCredentialExecutor> {
		let cwd = EnvPath::new(Str::new(cwd.to_string_lossy()))
			.unwrap_or_else(|_| EnvPath::new(Str::new_static(".")).expect("non-empty literal"));
		Arc::new(EnvCommandCredentialExecutor::new(client, cwd, Self::TIMEOUT, Self::MAX_STDOUT))
	}
}

/// Starts the consent-only AutoQA delivery worker once GitHub credentials
/// exist.
#[derive(Clone, Copy, Debug, Default)]
pub struct TelemetryDelivery;

impl omp_envd::TelemetryUpload for TelemetryDelivery {
	fn start(&self, index: Arc<TelemetryIndex>, credentials: Arc<GithubCredentialBridge>) {
		crate::telemetry_upload::start(index, credentials);
	}
}

/// Builds the baseline environment bridges for one project.
///
/// Core tools, Python registrations, and session routing are installed by the
/// environment and kernel composition directly; this helper carries the
/// optional host-resource authority plus the driver-owned command-credential
/// and telemetry-delivery seams.
#[must_use]
pub fn builtin(
	_root: &Path,
	_search: Arc<InferenceBridge>,
	_goal_control: AgentGoalControl,
	host_resources: Option<Arc<dyn omp_envd::HostResources>>,
) -> omp_envd::RegistryBridges {
	omp_envd::RegistryBridges {
		host_resources,
		command_credentials: Some(Arc::new(CommandCredentials)),
		telemetry_upload: Some(Arc::new(TelemetryDelivery)),
		..omp_envd::RegistryBridges::default()
	}
}
