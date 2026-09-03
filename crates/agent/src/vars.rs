//! Kernel-owned convars (ADR 0012): loop pacing knobs the turn loop reads
//! from the effective control plane.

use omp_core::Str;

/// How many queued steering asides one safe point consumes (pi
/// `steeringMode`).
#[derive(
	Clone,
	Copy,
	Debug,
	Default,
	Eq,
	PartialEq,
	strum::EnumString,
	strum::IntoStaticStr,
	strum::VariantNames,
)]
#[strum(serialize_all = "kebab-case")]
pub enum SteeringMode {
	/// One interjection per safe point; the rest wait for the next one.
	#[default]
	OneAtATime,
	/// Every queued interjection lands at the first safe point.
	All,
}

omp_con::con_enum!(SteeringMode);

omp_con::var! {
	/// Steering asides consumed per safe point: `one-at-a-time` (pi's
	/// default pacing) or `all`.
	pub static AI_STEERING_MODE = ai_steering_mode: SteeringMode {
		default: SteeringMode::OneAtATime,
		flags: archive | session,
	};
}

/// Tool names the effective `sv_tools` allowlist advertises; `None` means
/// every registered tool.
#[must_use]
pub fn tool_allowlist(con: Option<&omp_con::Ctx>) -> Option<Vec<Str>> {
	let roster = omp_con::SV_TOOLS.get(con?);
	(!roster.is_empty()).then_some(roster)
}
