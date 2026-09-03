//! Production composition for the journal-first headless agent kernel.

use std::{
	fs,
	path::{Path, PathBuf},
	sync::Arc,
	time::{Duration, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use omp_agent::{
	CanonicalPromptSource, DirectorRegistry, DispatchPolicy, ExtensionRegistrar,
	ExternalDispatchEvent, ExternalDispatchRequest, ExternalDispatchStream, ExternalToolExecutor,
	Kernel, RouteFacts, RuntimeFlags,
};
use omp_core::{SecretString, Str, Ulid};
use omp_dom::{Op, PropKey, Txn, Value};
use omp_inference::{
	CallMeta, ChatRequest, ChatStream, Client, ExecutionBudget, ProviderService, RequestId, Target,
	router::Router,
};
use omp_session::{ComponentRegistry, Session};
use omp_tool::{
	Abort, BlobRef as ToolBlobRef, Claims, Part as ToolPart, Precedence, Presentation, Registry,
};
use omp_tools::output_schema::SchemaMode;
use parking_lot::RwLock;

#[path = "con_journal.rs"]
mod con_journal;

use super::{HeadlessError, gateway::GatewayInference};
use crate::registry::{
	InferenceSessionOverrides, ProductionInference as ProductionStack,
	production_inference_for_session,
};

/// Stable prompt projection overrides supplied by one invocation.
#[derive(Clone, Debug, Default)]
pub struct PromptOverrides {
	/// Complete replacement for the customizable prompt bands.
	pub custom_prompt:          Option<Str>,
	/// Guidance appended after the stable prompt.
	pub append_prompt:          Option<Str>,
	/// Resolved personality prompt text.
	pub personality:            Option<Str>,
	/// Whether model identity is included.
	pub include_model:          Option<bool>,
	/// Whether workstation facts are included.
	pub include_workstation:    Option<bool>,
	/// Whether a bounded workspace tree is included.
	pub include_workspace_tree: Option<bool>,
	/// Whether Mermaid guidance is included.
	pub render_mermaid:         Option<bool>,
	/// Whether enabled skills are included.
	pub include_skills:         Option<bool>,
	/// Whether all provider prompt bands are bypassed.
	pub null_prompt:            bool,
}

/// Native extension-root composition for one invocation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NativeExtensionMode {
	/// Merge explicit roots with configured roots.
	#[default]
	Merge,
	/// Admit only explicit roots.
	ExplicitOnly,
	/// Disable native extension discovery.
	Disabled,
}

/// Driver-owned extension policy supplied by one invocation.
#[derive(Clone, Debug)]
pub struct LaunchExtensionPolicy {
	/// Ordered explicit native extension roots.
	pub native_roots:      Vec<PathBuf>,
	/// How explicit roots compose with configured roots.
	pub native_mode:       NativeExtensionMode,
	/// Whether workspace-owned roots participate.
	pub include_workspace: bool,
	/// Exact operator-trusted Python extension hosts.
	pub trusted:           Vec<omp_envd::worker::ExtHostSpec>,
	/// Declaration-owned CLI values delivered at activation.
	pub contributed:       Vec<omp_ext::config::ContributedCliValue>,
	/// Manifest setting overrides applied before environment attachment.
	pub setting_overrides: Vec<omp_ext::config::CliSettingOverride>,
}

impl Default for LaunchExtensionPolicy {
	fn default() -> Self {
		Self {
			native_roots:      Vec::new(),
			native_mode:       NativeExtensionMode::Merge,
			include_workspace: true,
			trusted:           Vec::new(),
			contributed:       Vec::new(),
			setting_overrides: Vec::new(),
		}
	}
}

/// Session selection and invocation-local production options.
#[derive(Clone, Default)]
pub struct KernelOptions {
	/// Resume the newest journal in the project session directory.
	pub continue_session:   bool,
	/// Open this exact journal path or session id.
	pub session:            Option<PathBuf>,
	/// Fork this journal path or session id into a fresh durable session.
	pub fork:               Option<PathBuf>,
	/// Override the project-native durable session directory.
	pub sessions_dir:       Option<PathBuf>,
	/// Create the journal in the system temporary directory.
	pub ephemeral:          bool,
	/// Disable every project tool while retaining normal inference discovery.
	pub no_tools:           bool,
	/// Restrict the registry to these validated stable tool names.
	pub tools:              Option<Vec<Str>>,
	/// Enable the Python evaluation tool in the project environment.
	pub py_eval:            bool,
	/// Detached environment daemon idle timeout.
	pub spawn_idle_timeout: Option<u64>,
	/// Invocation-only provider API key.
	pub api_key:            Option<SecretString>,
	/// Invocation-only approval policy.
	pub approval_mode:      Option<omp_envd::tool_settings::ApprovalMode>,
	/// Whether `model_selector` came from an explicit `--model`.
	pub model_override:     bool,
	/// Stable prompt projection overrides.
	pub prompt:             PromptOverrides,
	/// Invocation extension policy.
	pub extensions:         LaunchExtensionPolicy,
	/// Optional provider routing constraint.
	pub provider:           Option<omp_catalog::ProviderId>,
	/// Connected inference gateway used instead of local provider composition.
	pub gateway:            Option<tonic::transport::Channel>,
	/// Process-local live-session routing authority.
	pub sessions:           Option<Arc<crate::sessions::SessionRegistry>>,
	/// Human-readable routing name for this kernel.
	pub session_name:       Option<Str>,
	/// Explicit restricted registry for specialized child compositions.
	pub tool_registry:      Option<Arc<Registry>>,
	/// Child-specific structured output schema installed on `yield@2`.
	pub output_schema:      Option<serde_json::Value>,
	/// Enforcement mode for the child-specific output schema.
	pub schema_mode:        Option<SchemaMode>,
}

/// Removes a no-session journal regardless of which presentation adapter or
/// error path drops the composed kernel.
struct EphemeralJournal {
	path: PathBuf,
}

impl Drop for EphemeralJournal {
	fn drop(&mut self) {
		if let Err(error) = fs::remove_file(&self.path)
			&& error.kind() != std::io::ErrorKind::NotFound
		{
			tracing::warn!(journal = %self.path.display(), %error, "ephemeral journal cleanup failed");
		}
	}
}

/// Direct production inference client plus the authorities that keep its
/// environment and authentication stack alive.
pub struct ProductionInference {
	client:             Client<ProviderService, Router>,
	/// Call metadata as composed at launch; `ai_model` re-targets a copy.
	meta:               CallMeta,
	/// Model the client is currently targeted at.
	model:              omp_catalog::ModelKey,
	catalog:            Arc<omp_catalog::snapshot::Catalog>,
	_environment:       omp_envd::ProjectEnvironment,
	_stack:             ProductionStack,
	con:                Arc<omp_con::Ctx>,
	_python_components: Vec<omp_envd::exthost::PyComponent>,
	_ephemeral_journal: Option<EphemeralJournal>,
}

impl ProductionInference {
	/// Applies the control plane to the next call: `ai_model` re-targets the
	/// client when it names a different catalog model (ADR 0012: the convar
	/// is the live route), and `ai_thinking` sets the reasoning effort.
	fn apply_convars(&mut self, request: &mut ChatRequest) {
		let selector = omp_con::AI_MODEL.get(&self.con);
		if !selector.is_empty()
			&& let Ok(model) = resolve_model_selector(self.catalog.as_ref(), selector.as_str())
			&& model.as_str() != self.model.as_str()
		{
			let key = omp_catalog::ModelKey::from(model.as_str());
			let mut meta = self.meta.clone();
			meta.target = match &self.meta.target {
				Target::Provider { provider, .. } => {
					Target::Provider { provider: provider.clone(), model: key.clone() }
				},
				_ => Target::Model(key.clone()),
			};
			self.client.set_call_meta(meta);
			self.model = key;
		}
		if matches!(request.reasoning, omp_inference::Setting::Unset) {
			let thinking = omp_con::AI_THINKING.get(&self.con);
			request.reasoning = convar_reasoning(self.catalog.as_ref(), &self.model, &thinking);
		}
	}
}

/// Translates the `ai_thinking` convar into the canonical reasoning request
/// the catalog allows for `model` (ADR 0017: code branches on compiled
/// capabilities, never on model names).
///
/// The model's thinking policy and routing decide through
/// [`omp_catalog::ThinkingRouting::resolve`]: an effort above the ladder
/// clamps to the model ceiling, one between rungs clamps down, and `off` on a
/// model that cannot stop reasoning falls back to the catalog's default level
/// (or stays unset so the router applies `ai_default_thinking`). Models
/// without a thinking policy never carry a reasoning request; codecs then
/// spell the resolved effort per route (ADR 0022: one canonical request).
fn convar_reasoning(
	catalog: &omp_catalog::snapshot::Catalog,
	model: &omp_catalog::ModelKey<str>,
	thinking: &str,
) -> omp_inference::Setting<omp_inference::ReasoningRequest> {
	let Some(spec) = catalog.model(model) else {
		return omp_inference::Setting::Unset;
	};
	let Some(policy) = spec
		.thinking
		.as_ref()
		.and_then(|id| catalog.thinking_policy(id))
	else {
		return omp_inference::Setting::Unset;
	};
	let requested = match thinking.parse::<omp_catalog::ReasoningEffort>() {
		Ok(effort) => omp_catalog::ThinkingEffort::from(effort),
		Err(_) => {
			tracing::warn!(value = thinking, "ai_thinking is not a reasoning effort; ignored");
			return omp_inference::Setting::Unset;
		},
	};
	let wire_model = omp_catalog::WireModelId::from_ref(model.as_str());
	let effort = match spec
		.thinking_routing
		.resolve(policy, Some(requested), wire_model)
	{
		Ok(selection) => selection.effort,
		Err(_) => match policy.default_level {
			Some(level) => level,
			None => return omp_inference::Setting::Unset,
		},
	};
	omp_inference::Setting::Prefer(omp_inference::ReasoningRequest {
		visibility:          omp_inference::ReasoningVisibility::Visible,
		effort:              Some(effort.into()),
		max_tokens:          None,
		preserve_signatures: true,
	})
}

#[derive(Clone)]
struct EnvToolExecutor {
	client: omp_env::EnvClient,
}

impl ExternalToolExecutor for EnvToolExecutor {
	fn invoke(&self, request: ExternalDispatchRequest) -> ExternalDispatchStream {
		let client = self.client.clone();
		Box::pin(async_stream::stream! {
			let opened = client.invoke(omp_env::frame::InvokeTool {
				invocation_id: request.call_id.to_string(),
				name: request.identity.name.to_string(),
				rev: request.identity.rev.to_string(),
				deadline_ms: u64::try_from(request.blocking_limit.as_millis()).unwrap_or(u64::MAX),
				..Default::default()
			}).await;
			let mut invocation = match opened {
				Ok(invocation) => invocation,
				Err(source) => {
					tracing::warn!(%source, call_id = %request.call_id, "environment tool open failed");
					yield ExternalDispatchEvent::Aborted(Abort::Interrupted {
						reason: Str::new_static("environment tool open failed"),
					});
					return;
				},
			};
			match invocation.next_event().await {
				Ok(Some(omp_env::InvocationEvent::Accepted(_))) => {},
				Ok(_) => {
					yield ExternalDispatchEvent::Aborted(Abort::InputDropped);
					return;
				},
				Err(source) => {
					tracing::warn!(%source, call_id = %request.call_id, "environment tool acceptance failed");
					yield ExternalDispatchEvent::Aborted(Abort::Interrupted {
						reason: Str::new_static("environment tool acceptance failed"),
					});
					return;
				},
			}
			let token = Bytes::from(Ulid::generate().to_string());
			let authorized_at_ms = SystemTime::now()
				.duration_since(UNIX_EPOCH)
				.map_or(0, |duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX));
			if let Err(source) = invocation
				.commit_args(Bytes::copy_from_slice(request.args.get().as_bytes()), token, authorized_at_ms, None)
				.await
			{
				tracing::warn!(%source, call_id = %request.call_id, "environment tool commit failed");
				yield ExternalDispatchEvent::Aborted(Abort::Interrupted {
					reason: Str::new_static("environment tool commit failed"),
				});
				return;
			}
			// ADR 0011: the stop request is forwarded to the environment, which
			// interrupts the unit (TERM, `sv_interrupt_grace`, KILL) and reports
			// the unit's own verdict; the dispatcher bounds how long that report
			// may take. Dropping this stream cancels the request as well.
			let mut interrupted = false;
			loop {
				let next = tokio::select! {
					biased;
					() = request.cancellation.cancelled(), if !interrupted => {
						interrupted = true;
						if let Err(source) = invocation.interrupt(Str::new_static("interrupted")).await {
							tracing::warn!(%source, call_id = %request.call_id, "environment tool interrupt failed");
							yield ExternalDispatchEvent::Aborted(Abort::EffectsUnknown {
								reason: Str::new_static("environment tool interrupt failed"),
							});
							return;
						}
						continue;
					},
					next = invocation.next_event() => next,
				};
				match next {
					Ok(Some(omp_env::InvocationEvent::Accepted(_))) => {},
					Ok(Some(omp_env::InvocationEvent::Admission(query))) => {
						if let Err(source) = invocation.admit(omp_env::frame::Admission {
							invocation_id: query.invocation_id,
							allow: true,
							..Default::default()
						}).await {
							tracing::warn!(%source, call_id = %request.call_id, "environment tool admission failed");
							yield ExternalDispatchEvent::Aborted(Abort::Interrupted {
								reason: Str::new_static("environment tool admission failed"),
							});
							return;
						}
					},
					Ok(Some(omp_env::InvocationEvent::Update(update))) => {
						match raw_json(update.json) {
							Ok(update) => yield ExternalDispatchEvent::Update(update),
							Err(()) => {
								yield ExternalDispatchEvent::Aborted(Abort::MissingOutcome);
								return;
							},
						}
					},
					Ok(Some(omp_env::InvocationEvent::Verdict(verdict))) => {
						let outcome = match raw_json(verdict.json) {
							Ok(outcome) => outcome,
							Err(()) => {
								yield ExternalDispatchEvent::Aborted(Abort::MissingOutcome);
								return;
							},
						};
						let mut parts = verdict.parts.into_iter().filter_map(tool_part).collect::<Vec<_>>();
						if parts.is_empty() {
							parts = structured_parts(outcome.get());
						}
						if verdict.is_error && parts.is_empty() {
							parts.push(ToolPart::Text { text: Str::new(outcome.get()) });
						}
						yield ExternalDispatchEvent::Done {
							outcome,
							parts,
							is_error: verdict.is_error,
						};
						return;
					},
					Ok(None) => {
						yield ExternalDispatchEvent::Aborted(Abort::MissingOutcome);
						return;
					},
					Err(source) => {
						tracing::warn!(%source, call_id = %request.call_id, "environment tool stream failed");
						yield ExternalDispatchEvent::Aborted(Abort::Interrupted {
							reason: Str::new_static("environment tool stream failed"),
						});
						return;
					},
				}
			}
		})
	}
}

fn raw_json(bytes: Bytes) -> Result<Box<serde_json::value::RawValue>, ()> {
	let text = String::from_utf8(bytes.to_vec()).map_err(|_| ())?;
	serde_json::value::RawValue::from_string(text).map_err(|_| ())
}

fn structured_parts(outcome: &str) -> Vec<ToolPart> {
	let Ok(value) = serde_json::from_str::<serde_json::Value>(outcome) else {
		return Vec::new();
	};
	value
		.get("value")
		.and_then(|value| value.get("parts"))
		.and_then(serde_json::Value::as_array)
		.into_iter()
		.flatten()
		.filter_map(|part| match part.get("kind").and_then(serde_json::Value::as_str) {
			Some("text") => part
				.get("text")
				.and_then(serde_json::Value::as_str)
				.map(|text| ToolPart::Text { text: Str::new(text) }),
			Some("json") => part
				.get("json")
				.map(|json| ToolPart::Json { json: Bytes::from(json.to_string()) }),
			_ => None,
		})
		.collect()
}

fn tool_part(part: omp_proto::thread::v1::Part) -> Option<ToolPart> {
	match part.kind? {
		omp_proto::thread::v1::part::Kind::Text(text) => {
			Some(ToolPart::Text { text: Str::new(text) })
		},
		omp_proto::thread::v1::part::Kind::Thinking(thinking) => {
			Some(ToolPart::Text { text: Str::new(thinking.text) })
		},
		omp_proto::thread::v1::part::Kind::Blob(blob) => Some(ToolPart::Blob {
			blob: ToolBlobRef {
				hash:       Str::new(omp_core::hex::encode(&blob.hash).to_string()),
				media_type: Str::new(blob.mime),
				byte_len:   blob.size,
			},
			alt:  None,
		}),
		omp_proto::thread::v1::part::Kind::Fallback(_)
		| omp_proto::thread::v1::part::Kind::ServerTool(_) => None,
	}
}

impl omp_agent::Inference for ProductionInference {
	fn chat(
		&mut self,
		mut request: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, omp_inference::Error>> + Send {
		self.apply_convars(&mut request);
		self.client.execute(request)
	}

	fn install_retry_sink(&mut self, sink: omp_inference::RetrySink) {
		// Both the launch metadata (the base every `ai_model` re-target copies)
		// and the client's live copy carry the sink.
		self.meta.response_hooks = self.meta.response_hooks.clone().with_retry_sink(sink);
		let mut live = self.client.call_meta().clone();
		live.response_hooks = self.meta.response_hooks.clone();
		self.client.set_call_meta(live);
	}
}

/// Inference selected by one headless invocation.
pub enum ComposedInference {
	/// Direct production provider stack.
	Production(ProductionInference),
	/// Remote inference gateway plus its local project-tool authority.
	Gateway {
		/// Raw gateway turn adapter.
		inference:          GatewayInference,
		/// Environment owner retained for local tool execution.
		_environment:       omp_envd::ProjectEnvironment,
		/// Live Python Component reducers retained for the controller lifetime.
		_python_components: Vec<omp_envd::exthost::PyComponent>,
		/// No-session journal cleanup owner.
		_ephemeral_journal: Option<EphemeralJournal>,
	},
}

impl ComposedInference {
	fn retain_ephemeral_journal(&mut self, journal: EphemeralJournal) {
		match self {
			Self::Production(inference) => inference._ephemeral_journal = Some(journal),
			Self::Gateway { _ephemeral_journal, .. } => *_ephemeral_journal = Some(journal),
		}
	}

	/// Borrows the project environment client retained by this composition.
	#[must_use]
	pub fn environment_client(&self) -> &omp_env::EnvClient {
		match self {
			Self::Production(inference) => inference._environment.client(),
			Self::Gateway { _environment, .. } => _environment.client(),
		}
	}

	/// Borrows the project environment retained by this composition (MCP
	/// inspection, extension reload).
	#[must_use]
	pub const fn environment(&self) -> &omp_envd::ProjectEnvironment {
		match self {
			Self::Production(inference) => &inference._environment,
			Self::Gateway { _environment, .. } => _environment,
		}
	}

	/// Borrows the production authentication and usage stack; `None` behind
	/// a remote gateway, whose credentials live on the gateway host.
	#[must_use]
	pub const fn production_stack(&self) -> Option<&ProductionStack> {
		match self {
			Self::Production(inference) => Some(&inference._stack),
			Self::Gateway { .. } => None,
		}
	}

	/// Catalog snapshot the composition routes through; `None` behind a
	/// remote gateway.
	#[must_use]
	pub const fn catalog(&self) -> Option<&Arc<omp_catalog::snapshot::Catalog>> {
		match self {
			Self::Production(inference) => Some(&inference.catalog),
			Self::Gateway { .. } => None,
		}
	}
}

impl omp_agent::Inference for ComposedInference {
	fn chat(
		&mut self,
		request: ChatRequest,
	) -> impl Future<Output = Result<ChatStream, omp_inference::Error>> + Send {
		async move {
			match self {
				Self::Production(inference) => inference.chat(request).await,
				Self::Gateway { inference, .. } => inference.chat(request).await,
			}
		}
	}

	fn install_retry_sink(&mut self, sink: omp_inference::RetrySink) {
		match self {
			Self::Production(inference) => inference.install_retry_sink(sink),
			Self::Gateway { inference, .. } => inference.install_retry_sink(sink),
		}
	}
}

/// Concrete prompt projection returned by [`compose_kernel`].
pub type PromptSource = CanonicalPromptSource;

fn install_yield_contract(
	registry: Arc<Registry>,
	output_schema: Option<&serde_json::Value>,
	schema_mode: Option<SchemaMode>,
) -> Result<Arc<Registry>, HeadlessError> {
	let Some(output_schema) = output_schema else {
		return Ok(registry);
	};
	let retained = registry
		.live_identities()
		.filter(|(name, _)| name.as_str() != "yield")
		.map(|(name, _)| name.clone())
		.collect::<Vec<_>>();
	let mut registry = registry.restrict(retained.iter().map(Str::as_str));
	registry.register(
		omp_tools::yield_tool::tool_for_schema(output_schema, schema_mode.unwrap_or_default())?,
		Presentation::Hidden,
		Claims {
			precedence: Precedence::CORE,
			claimant:   Str::new_static("omp/core"),
			replaces:   None,
		},
	)?;
	Ok(Arc::new(registry))
}

/// Composes the production environment, inference route, tools, prompt, and
/// authoritative `.oms` session for a headless command.
pub async fn compose_kernel(
	data_dir: &Path,
	project_root: &Path,
	model_selector: &str,
	ctx: Arc<omp_con::Ctx>,
	options: KernelOptions,
) -> Result<(Kernel<ComposedInference>, Session, PromptSource), HeadlessError> {
	let project_root = fs::canonicalize(project_root)?;
	let state_dir = omp_env::project_state::directory(data_dir, &project_root)?;
	let sessions_dir = options
		.sessions_dir
		.clone()
		.unwrap_or_else(|| state_dir.join("sessions"));
	fs::create_dir_all(&sessions_dir)?;
	let tools_enabled = !options.no_tools;
	let live_sessions = options
		.sessions
		.clone()
		.unwrap_or_else(|| Arc::new(crate::sessions::SessionRegistry::new()));
	let bridges = if tools_enabled {
		omp_envd::RegistryBridges {
			dynamic_tools: vec![
				omp_envd::DynamicTool::new(
					omp_tools::task::tool(crate::subagent::spawn::TaskDeclarationSpawner),
					omp_tool::Presentation::Slot,
					omp_tool::Claims {
						precedence: omp_tool::Precedence::CORE,
						claimant:   Str::new_static("omp/core"),
						replaces:   None,
					},
				),
				omp_envd::DynamicTool::new(
					omp_tools::hub::tool(crate::subagent::hub::HubDeclarationBackend),
					omp_tool::Presentation::Slot,
					omp_tool::Claims {
						precedence: omp_tool::Precedence::CORE,
						claimant:   Str::new_static("omp/core"),
						replaces:   None,
					},
				),
			],
			..omp_envd::RegistryBridges::default()
		}
	} else {
		omp_envd::RegistryBridges::default()
	};

	let mut trusted_extensions = options.extensions.trusted.clone();
	for extension in &mut trusted_extensions {
		omp_ext::config::apply_resolved_setting_overrides(
			extension.key.extension().as_str(),
			&extension.manifest.setting_schemas,
			&mut extension.settings,
			&options.extensions.setting_overrides,
		)?;
	}
	let native_mode = match options.extensions.native_mode {
		NativeExtensionMode::Merge => crate::discovery::native::NativeLoadMode::Merge,
		NativeExtensionMode::ExplicitOnly => crate::discovery::native::NativeLoadMode::ExplicitOnly,
		NativeExtensionMode::Disabled => crate::discovery::native::NativeLoadMode::Disabled,
	};
	let home = if native_mode == crate::discovery::native::NativeLoadMode::Merge {
		omp_core::dirs::home_dir().ok_or(omp_core::dirs::DataDirError::HomeUnset)?
	} else {
		project_root.clone()
	};
	let native_extensions = crate::discovery::native::admit_native_extensions(
		&project_root,
		&home,
		crate::discovery::native::NativeAdmissionOptions {
			explicit_roots:    &options.extensions.native_roots,
			mode:              native_mode,
			include_workspace: options.extensions.include_workspace,
			setting_overrides: &options.extensions.setting_overrides,
		},
	)?;
	trusted_extensions.extend(
		native_extensions
			.into_iter()
			.map(|extension| extension.spec),
	);
	let environment =
		omp_envd::ProjectEnvironment::attach(&project_root, &state_dir, omp_envd::AttachOptions {
			py_eval: options.py_eval,
			approval_mode: options.approval_mode,
			trusted_extensions,
			contributed_values: options.extensions.contributed.clone(),
			con: Arc::clone(&ctx),
			bridges,
			spawn_idle_timeout: options.spawn_idle_timeout,
		})
		.await?;
	let admission_gate = environment.admission_gate();
	let hub_environment = environment.client().clone();
	let mut component_registry = ComponentRegistry::standard();
	let mut director_registry = DirectorRegistry::standard();
	let mut extension_registrar = ExtensionRegistrar::new();
	let python_components = environment.register_python_extensions(&mut extension_registrar)?;
	let live_python_components = python_components.clone();
	let _installed = extension_registrar.install(&mut director_registry, &mut component_registry);

	let complete_registry = options
		.tool_registry
		.clone()
		.unwrap_or_else(|| environment.registry());
	let registry = if options.no_tools {
		if options.tools.is_some() {
			return Err(
				std::io::Error::new(
					std::io::ErrorKind::InvalidInput,
					"--tools and --no-tools are mutually exclusive",
				)
				.into(),
			);
		}
		Arc::new(Registry::new())
	} else if let Some(names) = &options.tools {
		validate_tool_names(&complete_registry, names)?;
		Arc::new(complete_registry.restrict(names.iter().map(Str::as_str)))
	} else {
		complete_registry
	};
	let registry =
		install_yield_contract(registry, options.output_schema.as_ref(), options.schema_mode)?;

	let catalog = if options.gateway.is_some() {
		Arc::new(omp_catalog::snapshot::Catalog::embedded().clone())
	} else {
		crate::registry::production_catalog(data_dir)?
	};
	let model = resolve_model_selector(catalog.as_ref(), model_selector)?;
	let model_key = omp_catalog::ModelKey::from(model.as_str());
	let model_spec = catalog
		.model(&model_key)
		.ok_or_else(|| HeadlessError::UnknownModel { selector: model.clone() })?;
	let route_facts = route_facts(catalog.as_ref(), model_spec);
	let external = Arc::new(EnvToolExecutor { client: environment.client().clone() });

	let mut inference = if let Some(channel) = options.gateway {
		ComposedInference::Gateway {
			inference:          GatewayInference::new(channel, model.as_str()),
			_environment:       environment,
			_python_components: python_components,
			_ephemeral_journal: None,
		}
	} else {
		let stack = production_inference_for_session(
			data_dir,
			Arc::clone(&registry),
			Some(&project_root),
			InferenceSessionOverrides {
				provider: options.api_key.as_ref().and(options.provider.clone()),
				api_key: options.api_key,
				con: Some(Arc::clone(&ctx)),
				..InferenceSessionOverrides::default()
			},
		)
		.await?;
		let planner = Router::new(stack.registry.clone(), Duration::from_secs(30));
		let target = match options.provider {
			Some(provider) => Target::Provider { provider, model: model_key },
			None => Target::Model(model_key),
		};
		let meta = CallMeta {
			id: RequestId::from(format!("omp-print-{}", Ulid::generate())),
			target,
			deadline: None,
			budget: ExecutionBudget::default(),
			session: None,
			response_hooks: Default::default(),
		};
		let client = Client::new(stack.registry.service(), planner, meta.clone());
		ComposedInference::Production(ProductionInference {
			client,
			meta,
			model: omp_catalog::ModelKey::from(model.as_str()),
			catalog: Arc::clone(&catalog),
			_environment: environment,
			_stack: stack,
			con: Arc::clone(&ctx),
			_python_components: python_components,
			_ephemeral_journal: None,
		})
	};

	let terminal = terminal_identity();
	let journal_path = select_journal_path(
		&sessions_dir,
		options.session.as_deref(),
		options.fork.as_deref(),
		options.continue_session,
		options.ephemeral,
		terminal.as_deref(),
	)?;
	let ephemeral_journal = options
		.ephemeral
		.then(|| EphemeralJournal { path: journal_path.clone() });
	let mut session = if journal_path.exists() {
		Session::open(&journal_path, component_registry)?
	} else {
		if let Some(parent) = journal_path.parent() {
			fs::create_dir_all(parent)?;
		}
		Session::create(&journal_path, component_registry)?
	};
	let con_journal = Arc::new(con_journal::ConJournal::attach(Arc::clone(&ctx), session.dom()));
	apply_model_override(&ctx, model.as_str(), options.model_override)?;
	install_prompt_facts(&mut session, &project_root, model.as_str(), &options.prompt)?;
	if !options.ephemeral {
		remember_terminal_session(&sessions_dir, terminal.as_deref(), &journal_path)?;
	}
	if let Some(ephemeral_journal) = ephemeral_journal {
		inference.retain_ephemeral_journal(ephemeral_journal);
	}

	let prompt = CanonicalPromptSource;
	let spill = omp_journal::blob::BlobStore::open(data_dir.join("artifacts"))?;
	// The environment applies `sv_interrupt_grace` between TERM and KILL; the
	// dispatcher grants that courtesy plus one second for the unit's verdict
	// to travel back before it forces the call closed as effects-unknown.
	let unit_grace = omp_envd::host_settings::SV_INTERRUPT_GRACE
		.get(&ctx)
		.to_std()?;
	let policy = DispatchPolicy::new(spill)
		.with_interrupt_grace(unit_grace.saturating_add(Duration::from_secs(1)));
	let runtime_flags = RuntimeFlags {
		automatic_compaction:     ctx
			.get("ai_compaction_enabled")
			.and_then(|value| match value {
				omp_con::Value::Bool(value) => Some(value),
				_ => None,
			})
			.unwrap_or(true),
		goal_enabled:             ctx
			.get("cl_goal_enabled")
			.and_then(|value| match value {
				omp_con::Value::Bool(value) => Some(value),
				_ => None,
			})
			.unwrap_or(true),
		autolearn_enabled:        ctx
			.get("ai_autolearn_enabled")
			.and_then(|value| match value {
				omp_con::Value::Bool(value) => Some(value),
				_ => None,
			})
			.unwrap_or(false),
		autolearn_min_tool_calls: ctx
			.get("ai_autolearn_min_tool_calls")
			.and_then(|value| match value {
				omp_con::Value::Int(value) => usize::try_from(value).ok(),
				_ => None,
			})
			.unwrap_or(5),
		recover_inline_edits:     ctx
			.get("sv_edit_recover_inline_edits")
			.and_then(|value| match value {
				omp_con::Value::Bool(value) => Some(value),
				_ => None,
			})
			.unwrap_or(true),
	};
	let mut kernel = Kernel::new(inference, registry, policy, prompt)
		.with_director_registry(director_registry)
		.with_external_executor(external)
		.with_route_facts(route_facts)
		.with_runtime_flags(runtime_flags)
		.with_con_context(Arc::clone(&ctx))
		.with_hook_gate(admission_gate)
		.with_session_state_bridge(con_journal.clone());
	kernel.register_live_component(con_journal.live_component());
	for component in live_python_components {
		kernel.register_live_component(Box::new(component));
	}
	kernel = kernel
		.with_session_authority(Arc::clone(&live_sessions) as Arc<dyn omp_agent::SessionAuthority>);
	let id = journal_path
		.file_stem()
		.and_then(|name| name.to_str())
		.map_or_else(|| Str::new(Ulid::generate().to_string()), Str::new);
	let name = options.session_name.clone().unwrap_or_else(|| id.clone());
	live_sessions.register(name.clone(), crate::sessions::KernelHandle {
		id:       crate::sessions::SessionId::new(id),
		name:     name.clone(),
		up:       kernel.mailbox(),
		snapshot: Arc::new(RwLock::new(session.dom().snapshot())),
	});
	if tools_enabled {
		let cfg_root = project_root.join(".omp");
		let cfg: Arc<dyn omp_con::CfgLoader> =
			Arc::new(move |name: &str| fs::read_to_string(cfg_root.join(name)).ok().map(Str::new));
		kernel = kernel
			.with_session_tool(Arc::new(crate::subagent::spawn::TaskSessionTool::new(
				data_dir.to_path_buf(),
				project_root.clone(),
				sessions_dir,
				Arc::clone(&live_sessions),
				Arc::clone(&ctx),
				cfg,
				hub_environment.clone(),
				name.clone(),
				model,
			)))
			.with_session_tool(Arc::new(crate::subagent::hub::HubSessionTool::new(
				hub_environment,
				project_root.clone(),
				name,
			)));
	}
	Ok((kernel, session, prompt))
}

/// Facts fixed at composition that in-chat session switches (`/new`,
/// `/resume`, `/fork`, `/drop`) reuse: where journals live, which project
/// and model the prompt facts name, and the live-session routing index the
/// switched-in session registers with.
#[derive(Clone)]
pub struct SessionHome {
	/// Directory holding this project's `.oms` journals.
	pub sessions_dir: PathBuf,
	/// Canonical project root recorded in prompt facts.
	pub project_root: PathBuf,
	/// Resolved model key recorded in prompt facts.
	pub model:        Str,
	/// Invocation prompt projection overrides.
	pub prompt:       PromptOverrides,
	/// Process-local live-session routing authority.
	pub live:         Arc<crate::sessions::SessionRegistry>,
	/// The kernel's upward mailbox, shared by every session it drives.
	pub up:           flume::Sender<omp_agent::Up>,
}

impl SessionHome {
	/// Resolves the session directory exactly as [`compose_kernel`] does.
	pub fn new(
		data_dir: &Path,
		project_root: &Path,
		options: &KernelOptions,
		model: Str,
		up: flume::Sender<omp_agent::Up>,
	) -> Result<Self, HeadlessError> {
		let project_root = fs::canonicalize(project_root)?;
		let state_dir = omp_env::project_state::directory(data_dir, &project_root)?;
		let sessions_dir = options
			.sessions_dir
			.clone()
			.unwrap_or_else(|| state_dir.join("sessions"));
		fs::create_dir_all(&sessions_dir)?;
		let live = options
			.sessions
			.clone()
			.unwrap_or_else(|| Arc::new(crate::sessions::SessionRegistry::new()));
		Ok(Self { sessions_dir, project_root, model, prompt: options.prompt.clone(), live, up })
	}

	/// Path of a fresh journal in the session directory.
	#[must_use]
	pub fn fresh_path(&self) -> PathBuf {
		self.sessions_dir.join(format!("{}.oms", Ulid::generate()))
	}

	/// Creates a new journal at `path` (or a fresh one), installs the prompt
	/// facts, and registers it as the live session.
	pub fn create(&self, path: Option<PathBuf>) -> Result<Session, HeadlessError> {
		let path = path.unwrap_or_else(|| self.fresh_path());
		if let Some(parent) = path.parent() {
			fs::create_dir_all(parent)?;
		}
		let mut session = Session::create(&path, ComponentRegistry::standard())?;
		install_prompt_facts(&mut session, &self.project_root, self.model.as_str(), &self.prompt)?;
		self.register(&session);
		Ok(session)
	}

	/// Opens an existing journal and registers it as the live session.
	pub fn open(&self, path: &Path) -> Result<Session, HeadlessError> {
		let path = resolve_session_path(&self.sessions_dir, path);
		let mut session = Session::open(&path, ComponentRegistry::standard())?;
		install_prompt_facts(&mut session, &self.project_root, self.model.as_str(), &self.prompt)?;
		self.register(&session);
		Ok(session)
	}

	/// Copies `source` to a fresh journal and opens the copy: the whole
	/// branch tree travels with the fork (the journal is the tree).
	pub fn fork(&self, source: &Path) -> Result<Session, HeadlessError> {
		let source = resolve_session_path(&self.sessions_dir, source);
		let path = self.fresh_path();
		fs::copy(source, &path)?;
		self.open(&path)
	}

	/// Registers (or re-registers) `session` under its journal stem.
	pub fn register(&self, session: &Session) {
		let id = session
			.journal_path()
			.file_stem()
			.and_then(|name| name.to_str())
			.map_or_else(|| Str::new(Ulid::generate().to_string()), Str::new);
		self
			.live
			.register(id.clone(), crate::sessions::KernelHandle {
				id:       crate::sessions::SessionId::new(id.clone()),
				name:     id,
				up:       self.up.clone(),
				snapshot: Arc::new(RwLock::new(session.dom().snapshot())),
			});
	}

	/// Removes `session`'s journal from the live index (before its file is
	/// deleted or the process switches away).
	pub fn unregister(&self, session: &Session) {
		if let Some(id) = session
			.journal_path()
			.file_stem()
			.and_then(|name| name.to_str())
		{
			self.live.remove(crate::sessions::SessionId::from_ref(id));
		}
	}
}

/// Resolves a session selector the way `--resume` does: a bare id is a
/// stem in the session directory, anything with a directory or extension
/// is a path.
fn resolve_session_path(sessions_dir: &Path, path: &Path) -> PathBuf {
	if path.components().count() > 1 || path.extension().is_some() {
		path.to_path_buf()
	} else {
		sessions_dir.join(path).with_extension("oms")
	}
}

fn apply_model_override(
	ctx: &omp_con::Ctx,
	model: &str,
	explicit: bool,
) -> Result<(), HeadlessError> {
	if explicit {
		omp_con::AI_MODEL
			.set(ctx, Str::new(model))
			.map_err(|error| std::io::Error::other(error))?;
	}
	Ok(())
}

fn route_facts(
	catalog: &omp_catalog::snapshot::Catalog,
	model: &omp_catalog::ModelSpec,
) -> RouteFacts {
	RouteFacts {
		// `forced_choice` is capability, not cost. Only an affirmative
		// penalty-free named-choice fact skips ADR 0019's soft escalation.
		forced_choice_free: catalog
			.wire_policy(&model.wire_policy)
			.and_then(|policy| policy.tool.named_choice)
			.unwrap_or(false),
		context_window:     model.limits.context_window.unwrap_or(0),
		image_input:        model
			.capabilities
			.chat
			.as_ref()
			.and_then(|chat| chat.input_modalities.constraints())
			.is_some_and(|modalities| {
				modalities.contains(omp_catalog::capability::ModalityBits::IMAGE)
			}),
	}
}

#[derive(Debug, thiserror::Error)]
#[error("unknown tool `{name}` in --tools allow-list")]
struct UnknownTool {
	name: Str,
}

fn validate_tool_names(registry: &Registry, names: &[Str]) -> Result<(), HeadlessError> {
	for name in names {
		if registry.live_identity(name.as_str()).is_none() {
			return Err(HeadlessError::Io(std::io::Error::new(
				std::io::ErrorKind::InvalidInput,
				UnknownTool { name: name.clone() },
			)));
		}
	}
	Ok(())
}

fn resolve_model_selector(
	catalog: &omp_catalog::snapshot::Catalog,
	selector: &str,
) -> Result<Str, HeadlessError> {
	if let Some(model) = catalog.model(omp_catalog::ModelKey::from_ref(selector)) {
		return Ok(Str::new(model.key.as_str()));
	}
	if let Some(model) = catalog.resolve_alias(selector) {
		return Ok(Str::new(model.key.as_str()));
	}
	Err(HeadlessError::UnknownModel { selector: Str::new(selector) })
}

fn terminal_identity() -> Option<Str> {
	let environment = [
		"OMP_TERMINAL_ID",
		"TERM_SESSION_ID",
		"WEZTERM_PANE",
		"KITTY_WINDOW_ID",
		"WT_SESSION",
		"TMUX_PANE",
		"SSH_TTY",
		"TTY",
	]
	.into_iter()
	.find_map(|name| {
		let value = std::env::var_os(name)?;
		let value = value.to_string_lossy();
		(!value.is_empty()).then(|| Str::new(value.as_ref()))
	});
	environment.or_else(terminal_device_identity)
}

#[cfg(unix)]
fn terminal_device_identity() -> Option<Str> {
	use std::io::IsTerminal as _;
	if !std::io::stdin().is_terminal() {
		return None;
	}
	["/dev/fd/0", "/proc/self/fd/0"].into_iter().find_map(|fd| {
		let path = fs::canonicalize(fd).ok()?;
		path
			.starts_with("/dev/")
			.then(|| Str::new(path.to_string_lossy()))
	})
}

#[cfg(not(unix))]
const fn terminal_device_identity() -> Option<Str> {
	None
}

fn terminal_marker(sessions_dir: &Path, terminal: &str) -> PathBuf {
	let key = omp_core::Hash32::sum(terminal.as_bytes()).to_hex();
	sessions_dir.join(".continue").join(key.as_str())
}

fn remembered_terminal_session(
	sessions_dir: &Path,
	terminal: Option<&str>,
) -> Result<Option<PathBuf>, HeadlessError> {
	let Some(terminal) = terminal else {
		return Ok(None);
	};
	let marker = terminal_marker(sessions_dir, terminal);
	let name = match fs::read_to_string(marker) {
		Ok(name) => name,
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
		Err(error) => return Err(error.into()),
	};
	let name = name.trim();
	let relative = Path::new(name);
	if name.is_empty()
		|| relative.components().count() != 1
		|| relative
			.extension()
			.and_then(|extension| extension.to_str())
			!= Some(omp_journal::FILE_EXTENSION)
	{
		return Ok(None);
	}
	let path = sessions_dir.join(relative);
	Ok(path.is_file().then_some(path))
}

fn remember_terminal_session(
	sessions_dir: &Path,
	terminal: Option<&str>,
	journal: &Path,
) -> Result<(), HeadlessError> {
	let Some(terminal) = terminal else {
		return Ok(());
	};
	let Ok(relative) = journal.strip_prefix(sessions_dir) else {
		return Ok(());
	};
	if relative.components().count() != 1 {
		return Ok(());
	}
	let Some(name) = relative.file_name() else {
		return Ok(());
	};
	let marker = terminal_marker(sessions_dir, terminal);
	if let Some(parent) = marker.parent() {
		fs::create_dir_all(parent)?;
	}
	fs::write(marker, name.to_string_lossy().as_bytes())?;
	Ok(())
}

fn select_journal_path(
	sessions_dir: &Path,
	explicit: Option<&Path>,
	fork: Option<&Path>,
	continue_session: bool,
	ephemeral: bool,
	terminal: Option<&str>,
) -> Result<PathBuf, HeadlessError> {
	let selected = usize::from(explicit.is_some())
		+ usize::from(fork.is_some())
		+ usize::from(continue_session)
		+ usize::from(ephemeral);
	if selected > 1 {
		return Err(
			std::io::Error::new(
				std::io::ErrorKind::InvalidInput,
				"session, fork, continue, and ephemeral modes are mutually exclusive",
			)
			.into(),
		);
	}
	if let Some(path) = explicit {
		return Ok(resolve_session_path(sessions_dir, path));
	}
	if let Some(source) = fork {
		let source = resolve_session_path(sessions_dir, source);
		let destination = sessions_dir.join(format!("{}.oms", Ulid::generate()));
		fs::copy(source, &destination)?;
		return Ok(destination);
	}
	if continue_session && let Some(path) = remembered_terminal_session(sessions_dir, terminal)? {
		return Ok(path);
	}
	let name = format!("{}.oms", Ulid::generate());
	Ok(if ephemeral {
		std::env::temp_dir().join(name)
	} else {
		sessions_dir.join(name)
	})
}

fn install_prompt_facts(
	session: &mut Session,
	project_root: &Path,
	model: &str,
	overrides: &PromptOverrides,
) -> Result<(), omp_session::SessionError> {
	let home = std::env::var_os("HOME").map_or_else(|| project_root.to_path_buf(), PathBuf::from);
	let mut facts = serde_json::json!({
		"cwd": project_root.to_string_lossy(),
		"home": home.to_string_lossy(),
		"model": { "identifier": model, "codex_task_policy": false },
		"context_files": [],
		"date": jiff::Zoned::now().strftime("%Y-%m-%d").to_string(),
		"null_prompt": overrides.null_prompt,
	});
	let object = facts.as_object_mut().expect("prompt facts are an object");
	for (name, value) in [
		("custom_prompt", overrides.custom_prompt.as_ref()),
		("append_prompt", overrides.append_prompt.as_ref()),
		("personality", overrides.personality.as_ref()),
	] {
		if let Some(value) = value {
			object.insert(name.to_owned(), serde_json::Value::String(value.to_string()));
		}
	}
	for (name, value) in [
		("include_model", overrides.include_model),
		("include_workstation", overrides.include_workstation),
		("include_workspace_tree", overrides.include_workspace_tree),
		("render_mermaid", overrides.render_mermaid),
		("include_skills", overrides.include_skills),
	] {
		if let Some(value) = value {
			object.insert(name.to_owned(), serde_json::Value::Bool(value));
		}
	}
	let raw = serde_json::value::to_raw_value(&facts)?;
	session.patch(Txn {
		cause: session
			.head()
			.ok_or(omp_session::SessionError::NoActiveTurn)?,
		label: Some(Str::new_static("prompt.facts")),
		ops:   vec![Op::Set {
			h:     session.dom().meta(),
			prop:  PropKey::Custom(Str::new_static("prompt-facts")),
			value: Value::Json(raw),
		}],
	})?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use omp_catalog::{
		ModelKey, ReasoningEffort, ThinkingEffort, ThinkingPolicy, WireTarget, snapshot::Catalog,
	};
	use omp_inference::{
		ChatRequest, NegotiationPolicy, RequestId, Sampling, Setting,
		codec::{
			EncodeContext,
			openai_responses::{OpenAiResponsesCodec, OpenAiResponsesOptions},
		},
	};

	use super::{
		EphemeralJournal, HeadlessError, KernelOptions, PromptOverrides, apply_model_override,
		convar_reasoning, install_prompt_facts, install_yield_contract, remember_terminal_session,
		route_facts, select_journal_path, validate_tool_names,
	};

	const GPT5: &str = "openai/gpt-5";

	fn gpt5_policy(catalog: &Catalog) -> &ThinkingPolicy {
		let spec = catalog
			.model(ModelKey::from_ref(GPT5))
			.expect("embedded gpt-5");
		catalog
			.thinking_policy(spec.thinking.as_ref().expect("gpt-5 reasons"))
			.expect("gpt-5 thinking policy")
	}

	fn effort(setting: &Setting<omp_inference::ReasoningRequest>) -> Option<ReasoningEffort> {
		match setting {
			Setting::Unset => None,
			Setting::Require(value) | Setting::Prefer(value) => value.effort,
		}
	}

	/// Lowers the convar-derived request for gpt-5 through the planner's
	/// thinking resolution and the Responses codec, exactly as a live call
	/// would, and returns the serialized `reasoning` object.
	fn gpt5_wire_reasoning(catalog: &Catalog, thinking: &str) -> Option<serde_json::Value> {
		let key = ModelKey::from_ref(GPT5);
		let spec = catalog.model(key).expect("embedded gpt-5");
		let policy = gpt5_policy(catalog);
		let route = spec
			.routes
			.iter()
			.filter_map(|route| catalog.route(route))
			.find(|route| route.codec.as_str() == "openai-responses")
			.expect("gpt-5 Responses route");
		let wire_model = spec
			.wire_ids
			.iter()
			.find(|(candidate, _)| candidate == &route.id)
			.expect("gpt-5 wire id")
			.1
			.clone();
		let wire_policy = catalog
			.wire_policy(&spec.wire_policy)
			.expect("gpt-5 wire policy");
		let request = ChatRequest {
			messages:          Arc::from([]),
			tools:             Arc::from([]),
			hosted_tools:      Arc::from([]),
			tool_choice:       Setting::Unset,
			output:            Setting::Unset,
			reasoning:         convar_reasoning(catalog, key, thinking),
			verbosity:         Setting::Unset,
			cache_retention:   Setting::Unset,
			service_tier:      Setting::Unset,
			sampling:          Sampling::default(),
			max_output_tokens: None,
			top_logprobs:      None,
			safety:            Arc::from([]),
			negotiation:       NegotiationPolicy::default(),
			forced_call:       None,
		};
		let selection = effort(&request.reasoning).map(|effort| {
			spec
				.thinking_routing
				.resolve(policy, Some(effort.into()), &wire_model)
				.expect("convar effort resolves against the catalog")
		});
		let target = WireTarget {
			route: route.id.clone(),
			codec: route.codec.clone(),
			endpoint: route.endpoint.clone(),
			wire_model,
		};
		let request_id = RequestId::new("convar-thinking");
		let context = EncodeContext {
			request_id: &request_id,
			route,
			target: Some(&target),
			policy: wire_policy,
			thinking_policy: Some(policy),
			thinking_selection: selection.as_ref(),
			..EncodeContext::default()
		};
		let encoded = OpenAiResponsesCodec::new(OpenAiResponsesOptions::default())
			.encode_chat(&context, &request)
			.expect("gpt-5 request encodes");
		encoded
			.request
			.reasoning
			.as_ref()
			.map(|reasoning| serde_json::to_value(reasoning).expect("reasoning serializes"))
	}

	#[test]
	fn ai_thinking_off_never_sends_none_to_a_model_without_off() {
		let catalog = Catalog::embedded();
		let policy = gpt5_policy(catalog);
		assert!(
			!policy.efforts.contains(&ThinkingEffort::Off)
				&& policy.efforts.contains(&ThinkingEffort::Minimal),
			"gpt-5 ladder is minimal..high with no wire `none`: {:?}",
			policy.efforts
		);
		let reasoning = gpt5_wire_reasoning(catalog, "off");
		let effort = reasoning
			.as_ref()
			.and_then(|reasoning| reasoning.get("effort"))
			.cloned();
		assert_eq!(effort, None, "reasoning-off must not spell an effort: {reasoning:?}");
	}

	#[test]
	fn ai_thinking_above_the_ladder_clamps_to_the_catalog_ceiling() {
		let catalog = Catalog::embedded();
		let request = convar_reasoning(catalog, ModelKey::from_ref(GPT5), "xhigh");
		assert_eq!(effort(&request), Some(ReasoningEffort::High));
		let reasoning = gpt5_wire_reasoning(catalog, "xhigh").expect("reasoning object");
		assert_eq!(reasoning.get("effort"), Some(&serde_json::json!("high")));
	}

	#[test]
	fn ai_thinking_off_on_a_model_that_requires_effort_uses_the_catalog_default() {
		let catalog = Catalog::embedded();
		let (spec, policy) = catalog
			.models()
			.iter()
			.find_map(|spec| {
				let policy = catalog.thinking_policy(spec.thinking.as_ref()?)?;
				(policy.requires_effort == Some(true)).then_some((spec, policy))
			})
			.expect("embedded catalog has a model that cannot stop reasoning");
		assert!(!policy.supports(ThinkingEffort::Off));
		let request = convar_reasoning(catalog, &spec.key, "off");
		assert_eq!(
			effort(&request),
			policy.default_level.map(ReasoningEffort::from),
			"{}: off falls back to the catalog default level",
			spec.key
		);
	}

	#[test]
	fn ai_thinking_without_a_thinking_policy_leaves_reasoning_unset() {
		let catalog = Catalog::embedded();
		let spec = catalog
			.models()
			.iter()
			.find(|spec| spec.thinking.is_none())
			.expect("embedded catalog has a non-reasoning model");
		assert!(matches!(convar_reasoning(catalog, &spec.key, "high"), Setting::Unset));
	}

	#[test]
	fn explicit_model_replaces_the_restored_session_value() {
		let ctx = omp_con::Ctx::new();
		ctx.restore_session_write("ai_model", "archived/model")
			.expect("restored model");
		apply_model_override(&ctx, "explicit/model", true).expect("explicit model");
		assert_eq!(omp_con::AI_MODEL.get(&ctx).as_str(), "explicit/model");
		assert!(
			ctx.session_writes()
				.any(|(name, value)| name == "ai_model" && value.to_string() == "explicit/model")
		);

		let inherited = omp_con::Ctx::new();
		inherited
			.restore_session_write("ai_model", "archived/model")
			.expect("restored model");
		apply_model_override(&inherited, "default/model", false).expect("default model");
		assert_eq!(omp_con::AI_MODEL.get(&inherited).as_str(), "archived/model");
	}

	#[test]
	fn continue_is_terminal_local_and_missing_breadcrumb_starts_fresh() {
		let scratch = tempfile::tempdir().expect("tempdir");
		let sessions = scratch.path();
		let first = sessions.join("first.oms");
		std::fs::write(&first, b"journal").expect("journal");
		remember_terminal_session(sessions, Some("terminal-a"), &first).expect("breadcrumb");

		let resumed = select_journal_path(sessions, None, None, true, false, Some("terminal-a"))
			.expect("continue");
		assert_eq!(resumed, first);

		let fresh = select_journal_path(sessions, None, None, true, false, Some("terminal-b"))
			.expect("fresh fallback");
		assert_ne!(fresh, first);
		assert!(!fresh.exists());
		assert_eq!(fresh.parent(), Some(sessions));

		std::fs::remove_file(&first).expect("remove stale journal");
		let stale = select_journal_path(sessions, None, None, true, false, Some("terminal-a"))
			.expect("stale fallback");
		assert_ne!(stale, first);
		assert!(!stale.exists());
	}

	#[test]
	fn startup_fork_copies_to_a_fresh_project_journal() {
		let scratch = tempfile::tempdir().expect("tempdir");
		let source = scratch.path().join("source.oms");
		std::fs::write(&source, b"authoritative branch").expect("source");
		let fork = select_journal_path(
			scratch.path(),
			None,
			Some(std::path::Path::new("source")),
			false,
			false,
			None,
		)
		.expect("fork");
		assert_ne!(fork, source);
		assert_eq!(std::fs::read(fork).expect("fork bytes"), b"authoritative branch");
	}

	#[test]
	fn ephemeral_cleanup_runs_when_the_owner_drops() {
		let scratch = tempfile::tempdir().expect("tempdir");
		let path = scratch.path().join("ephemeral.oms");
		std::fs::write(&path, b"private transcript").expect("journal");
		drop(EphemeralJournal { path: path.clone() });
		assert!(!path.exists());
	}

	#[test]
	fn prompt_overrides_are_journaled_as_prompt_facts() {
		let scratch = tempfile::tempdir().expect("tempdir");
		let path = scratch.path().join("prompt.oms");
		let mut session =
			omp_session::Session::create(path, omp_session::ComponentRegistry::standard())
				.expect("session");
		let overrides = PromptOverrides {
			custom_prompt: Some(omp_core::Str::new_static("custom")),
			append_prompt: Some(omp_core::Str::new_static("append")),
			include_model: Some(false),
			null_prompt: true,
			..PromptOverrides::default()
		};
		install_prompt_facts(&mut session, scratch.path(), "provider/model", &overrides)
			.expect("prompt facts");
		let value = session
			.dom()
			.get(session.dom().meta())
			.and_then(|meta| {
				meta.prop(&omp_dom::PropKey::Custom(omp_core::Str::new_static("prompt-facts")))
			})
			.expect("prompt facts prop");
		let omp_dom::Value::Json(raw) = value else {
			panic!("prompt facts are structured JSON");
		};
		let facts: serde_json::Value = serde_json::from_str(raw.get()).expect("facts JSON");
		assert_eq!(facts["custom_prompt"], "custom");
		assert_eq!(facts["append_prompt"], "append");
		assert_eq!(facts["include_model"], false);
		assert_eq!(facts["null_prompt"], true);
	}

	#[test]
	fn forced_choice_capability_does_not_claim_penalty_free_routing() {
		let catalog = Catalog::embedded();
		let paid = catalog
			.models()
			.iter()
			.find(|model| {
				model.key.as_str().contains("claude")
					&& catalog
						.wire_policy(&model.wire_policy)
						.is_some_and(|policy| {
							policy.tool.forced_choice == Some(true)
								&& policy.tool.named_choice != Some(true)
						})
			})
			.expect("embedded Anthropic model has paid forced choice");
		assert!(!route_facts(catalog, paid).forced_choice_free);
	}

	#[test]
	fn ordinary_kernel_retains_generic_yield_contract() {
		let mut registry = omp_tool::Registry::new();
		registry
			.register(
				omp_tools::yield_tool::tool(),
				omp_tool::Presentation::Hidden,
				omp_tool::Claims {
					precedence: omp_tool::Precedence::CORE,
					claimant:   omp_core::Str::new_static("omp/core"),
					replaces:   None,
				},
			)
			.expect("generic yield");
		let registry = Arc::new(registry);
		let installed =
			install_yield_contract(Arc::clone(&registry), None, None).expect("ordinary registry");
		assert!(Arc::ptr_eq(&registry, &installed));
		assert!(matches!(
			installed.live_spec("yield").expect("yield").constraint,
			omp_tool::Constraint::None
		));
	}

	#[test]
	fn child_schema_replaces_generic_yield_before_registry_freeze() {
		let mut registry = omp_tool::Registry::new();
		registry
			.register(
				omp_tools::yield_tool::tool(),
				omp_tool::Presentation::Hidden,
				omp_tool::Claims {
					precedence: omp_tool::Precedence::CORE,
					claimant:   omp_core::Str::new_static("omp/core"),
					replaces:   None,
				},
			)
			.expect("generic yield");
		let installed = install_yield_contract(
			Arc::new(registry),
			Some(&serde_json::json!({
				"type": "object",
				"required": ["ok"],
				"properties": {"ok": {"type": "boolean"}},
			})),
			Some(omp_tools::output_schema::SchemaMode::Strict),
		)
		.expect("child yield contract");
		let spec = installed.live_spec("yield").expect("yield");
		assert!(matches!(spec.constraint, omp_tool::Constraint::Schema { priority: 100, .. }));
		assert_eq!(
			installed.presentation("yield").expect("presentation"),
			omp_tool::Presentation::Hidden
		);
		let schema: serde_json::Value =
			serde_json::from_slice(&spec.schema).expect("yield parameter schema");
		assert_eq!(
			schema["properties"]["result"]["oneOf"][0]["properties"]["data"]["anyOf"][0]["properties"]
				["ok"]["type"],
			"boolean"
		);
	}

	#[test]
	fn child_schema_installs_yield_in_an_otherwise_empty_registry() {
		let installed = install_yield_contract(
			Arc::new(omp_tool::Registry::new()),
			Some(&serde_json::json!({"type": "boolean"})),
			None,
		)
		.expect("child yield contract");
		assert_eq!(installed.live_identities().len(), 1);
		assert!(installed.live_identity("yield").is_some());
		assert!(matches!(
			installed.live_spec("yield").expect("yield").constraint,
			omp_tool::Constraint::None
		));
	}

	#[test]
	fn malformed_child_schema_fails_kernel_composition() {
		let result = install_yield_contract(
			Arc::new(omp_tool::Registry::new()),
			Some(&serde_json::json!(["not", "a", "schema"])),
			Some(omp_tools::output_schema::SchemaMode::Strict),
		);
		assert!(matches!(result, Err(HeadlessError::YieldSchema(_))));
	}

	#[test]
	fn unknown_tool_allow_list_is_rejected_before_kernel_construction() {
		let registry = omp_tool::Registry::new();
		let error =
			validate_tool_names(&registry, &[omp_core::Str::new_static("definitely-not-installed")])
				.expect_err("unknown tool");
		let HeadlessError::Io(error) = error else {
			panic!("tool validation is an invalid-input error");
		};
		assert!(error.to_string().contains("definitely-not-installed"));
	}

	#[test]
	fn approval_override_is_typed_in_kernel_options() {
		let options = KernelOptions {
			approval_mode: Some(omp_envd::tool_settings::ApprovalMode::AlwaysAsk),
			..KernelOptions::default()
		};
		assert_eq!(options.approval_mode, Some(omp_envd::tool_settings::ApprovalMode::AlwaysAsk));
	}
}
