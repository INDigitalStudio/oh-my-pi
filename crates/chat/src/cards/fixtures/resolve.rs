use super::{CardFixture, FixtureState};

/// Both devices take one `reason` argument (`envd::devices_host`
/// `proposal_schema`); the settled payload is the staged action's own.
const fn states(streaming: &'static str, args: &'static str) -> [FixtureState; 4] {
	[
		FixtureState { args: streaming, update: None, result: None, fault: None },
		FixtureState { args, update: None, result: None, fault: None },
		FixtureState { args, update: None, result: Some("{}"), fault: None },
		FixtureState { args, update: None, result: None, fault: Some(r#""Tool execution failed""#) },
	]
}

pub(super) const FIXTURES: &[CardFixture] = &[
	CardFixture {
		tool:   "resolve",
		title:  "",
		states: states(
			r#"{"reason":"The rename touches only"#,
			r#"{"reason":"The rename touches only tokens.ts and matches the request."}"#,
		),
	},
	CardFixture {
		tool:   "reject",
		title:  "",
		states: states(
			r#"{"reason":"The patch would also"#,
			r#"{"reason":"The patch would also delete the migration script."}"#,
		),
	},
];
