//! Derivation contracts: engagement layers as a projection of the Director
//! chain, session writes as a journaling stream, and whole-picture child
//! seeding.

use omp_con::{ConError, Ctx, DynamicVarSpec, Origin, TypeSpec, Value, VarFlags};
use omp_core::Str;

omp_con::var! {
	/// Session-journaled target.
	pub static DERIVED = test_derived: i32 {
		default: 1,
		flags: archive | session,
	};
	/// Archive-only target: never journaled, still seeds children.
	pub static ARCHIVED = test_archived_only: bool {
		default: false,
		flags: archive,
	};
}

fn chain(entries: &[(&str, &[(&str, i64)])]) -> Vec<(Str, Vec<(Str, Value)>)> {
	entries
		.iter()
		.map(|(owner, binds)| {
			(
				Str::new(*owner),
				binds
					.iter()
					.map(|(name, value)| (Str::new(*name), Value::Int(*value)))
					.collect(),
			)
		})
		.collect()
}

#[test]
fn derive_layers_replaces_the_stack_from_the_director_chain() {
	let ctx = Ctx::new();
	ctx.derive_layers(&chain(&[("plan#3", &[("test_derived", 7)])]));
	assert_eq!(ctx.get("test_derived"), Some(Value::Int(7)));
	assert_eq!(ctx.layer_owners(), vec![Str::new("plan#3")]);
	// Inner engagement wins; outer stays.
	ctx.derive_layers(&chain(&[("plan#3", &[("test_derived", 7)]), ("goal#5", &[(
		"test_derived",
		9,
	)])]));
	assert_eq!(ctx.get("test_derived"), Some(Value::Int(9)));
	// Rewind removed the inner element: derivation pops it without an exit call.
	ctx.derive_layers(&chain(&[("plan#3", &[("test_derived", 7)])]));
	assert_eq!(ctx.get("test_derived"), Some(Value::Int(7)));
	// Empty chain restores the session/default picture.
	ctx.derive_layers(&[]);
	assert_eq!(ctx.get("test_derived"), Some(Value::Int(1)));
	assert!(ctx.layer_owners().is_empty());
}

#[test]
fn derive_layers_is_a_no_op_for_an_unchanged_chain() {
	let ctx = Ctx::new();
	let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
	let counter = seen.clone();
	ctx.observe(move |_, _, _| {
		counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
	});
	let same = chain(&[("plan#3", &[("test_derived", 7)])]);
	ctx.derive_layers(&same);
	ctx.derive_layers(&same);
	ctx.derive_layers(&same);
	assert_eq!(seen.load(std::sync::atomic::Ordering::Relaxed), 1);
}

#[test]
fn session_writes_stream_carries_committed_values_not_engagement_values() {
	let ctx = Ctx::new();
	let rx = ctx.subscribe_session_writes();
	ctx.run("test_derived 4").unwrap();
	assert_eq!(rx.try_recv().unwrap(), (Str::new("test_derived"), Value::Int(4)));
	// An engagement bind changes the effective value but is not a session write.
	ctx.derive_layers(&chain(&[("plan#1", &[("test_derived", 9)])]));
	assert_eq!(ctx.get("test_derived"), Some(Value::Int(9)));
	assert!(rx.try_recv().is_err());
	// A shadowed user write still commits (and journals) the session value.
	ctx.run("test_derived 5").unwrap();
	assert_eq!(rx.try_recv().unwrap(), (Str::new("test_derived"), Value::Int(5)));
	assert_eq!(ctx.get("test_derived"), Some(Value::Int(9)));
	// Archive-only variables never enter the journal stream.
	ctx.run("test_archived_only true").unwrap();
	assert!(rx.try_recv().is_err());
	// A reset journals the default so replay clears the earlier write.
	ctx.set("test_derived", Value::Int(1), Origin::Default).unwrap();
	assert_eq!(rx.try_recv().unwrap(), (Str::new("test_derived"), Value::Int(1)));
}

#[test]
fn clear_session_write_drops_only_the_session_layer() {
	let ctx = Ctx::new();
	ctx.set("test_derived", Value::Int(2), Origin::Archive).unwrap();
	ctx.run("test_derived 6").unwrap();
	assert_eq!(ctx.get("test_derived"), Some(Value::Int(6)));
	let rx = ctx.subscribe_session_writes();
	ctx.clear_session_write("test_derived").unwrap();
	assert_eq!(ctx.get("test_derived"), Some(Value::Int(2)));
	assert!(rx.try_recv().is_err(), "clearing is derivation, not a write");
	assert!(matches!(
		ctx.clear_session_write("test_nonexistent"),
		Err(ConError::Unknown { .. })
	));
}

#[test]
fn seed_child_carries_every_diverging_variable_regardless_of_flags() {
	let parent = Ctx::new();
	parent.run("test_archived_only true").unwrap();
	parent.run("test_derived 3").unwrap();
	let seed = parent.seed_child();
	assert_eq!(seed.get("test_archived_only"), Some(&Value::Bool(true)));
	assert_eq!(seed.get("test_derived"), Some(&Value::Int(3)));
	// Values at their default are not restated.
	assert_eq!(seed.get("ai_fastmode"), None);
	// The seed is the parent's *effective* picture, engagement included.
	parent.derive_layers(&chain(&[("plan#1", &[("test_derived", 8)])]));
	assert_eq!(parent.seed_child().get("test_derived"), Some(&Value::Int(8)));
}

#[test]
fn seed_child_carries_dynamic_declarations_before_values() {
	let parent = Ctx::new();
	parent
		.register_dynamic_var(DynamicVarSpec {
			name:    Str::new_static("ext::demo::enabled"),
			desc:    Str::new_static("Enable the demo extension"),
			ty:      TypeSpec::BOOL,
			flags:   VarFlags::SESSION,
			default: Value::Bool(false),
		})
		.unwrap();

	let default_seed = parent.seed_child();
	assert_eq!(default_seed.dynamic_vars().len(), 1);
	assert_eq!(default_seed.get("ext::demo::enabled"), None);

	parent
		.set("ext::demo::enabled", Value::Bool(true), Origin::Session)
		.unwrap();
	let (declarations, values) = parent.seed_child().into_parts();
	let child = Ctx::new();
	for declaration in declarations {
		child.register_dynamic_var(declaration).unwrap();
	}
	for (name, value) in values {
		child.set(name.as_str(), value, Origin::Session).unwrap();
	}
	assert_eq!(child.get("ext::demo::enabled"), Some(Value::Bool(true)));
}
