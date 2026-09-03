//! Configured, symlink-confined vault authority for `vault://` resources.

use std::{
	collections::BTreeMap,
	fs, io,
	path::{Component, Path, PathBuf},
	sync::Arc,
};

use omp_core::{CowBytes, Str};
use parking_lot::RwLock;
use serde::Deserialize;
use toml::de;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VaultFile {
	#[serde(default)]
	vaults: BTreeMap<Str, PathBuf>,
}

/// The two `vaults.toml` files one process reads.
///
/// User configuration lives under `~/.o2`
/// ([`omp_core::dirs::user_config_root`], profile-aware) and never under the
/// data or state directory; project declarations live in
/// `<project>/.omp/vaults.toml` and shadow user vaults with the same name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultPaths {
	/// User-owned `<config root>/vaults.toml`.
	pub user:    PathBuf,
	/// Project-owned `<project>/.omp/vaults.toml`.
	pub project: PathBuf,
}

impl VaultPaths {
	/// Resolves both files from the user configuration root and the project
	/// root.
	#[must_use]
	pub fn new(user_config_root: &Path, project_root: &Path) -> Self {
		Self {
			user:    user_config_root.join("vaults.toml"),
			project: project_root.join(".omp/vaults.toml"),
		}
	}
}

#[derive(Clone, Debug, Default)]
pub struct VaultService {
	roots: Arc<RwLock<BTreeMap<Str, PathBuf>>>,
}

impl VaultService {
	/// Loads the effective vault authority: every user vault, shadowed by any
	/// project vault with the same name.
	pub fn load_layered(paths: &VaultPaths) -> Result<Self, VaultError> {
		let mut roots = parse_vaults(&paths.user)?;
		roots.extend(parse_vaults(&paths.project)?);
		Ok(Self { roots: Arc::new(RwLock::new(roots)) })
	}

	pub fn names(&self) -> Vec<Str> {
		self.roots.read().keys().cloned().collect()
	}

	fn target(&self, vault: &str, relative: &str, for_write: bool) -> Result<PathBuf, VaultError> {
		let root = self
			.roots
			.read()
			.get(vault)
			.cloned()
			.ok_or_else(|| VaultError::Unknown { name: Str::new(vault) })?;
		let relative = Path::new(relative);
		if relative.is_absolute()
			|| relative
				.components()
				.any(|c| !matches!(c, Component::Normal(_)))
		{
			return Err(VaultError::Escape);
		}
		let target = root.join(relative);
		let mut existing = if for_write {
			target.parent().unwrap_or(&root)
		} else {
			target.as_path()
		};
		while for_write && !existing.exists() {
			existing = existing.parent().ok_or(VaultError::Escape)?;
		}
		let canonical = existing
			.canonicalize()
			.map_err(|source| VaultError::Io { path: existing.to_path_buf(), source })?;
		if !canonical.starts_with(&root) {
			return Err(VaultError::Escape);
		}
		if for_write
			&& let Ok(metadata) = fs::symlink_metadata(&target)
			&& metadata.file_type().is_symlink()
		{
			return Err(VaultError::Escape);
		}
		Ok(target)
	}

	pub fn read(
		&self,
		vault: &str,
		relative: &str,
		limit: usize,
	) -> Result<CowBytes<'static>, VaultError> {
		let path = self.target(vault, relative, false)?;
		let metadata =
			fs::metadata(&path).map_err(|source| VaultError::Io { path: path.clone(), source })?;
		if metadata.len() > limit as u64 {
			return Err(VaultError::Limit { limit });
		}
		fs::read(&path)
			.map(CowBytes::from)
			.map_err(|source| VaultError::Io { path, source })
	}

	pub fn write(
		&self,
		vault: &str,
		relative: &str,
		bytes: &[u8],
		limit: usize,
	) -> Result<(), VaultError> {
		if bytes.len() > limit {
			return Err(VaultError::Limit { limit });
		}
		let path = self.target(vault, relative, true)?;
		let parent = path.parent().ok_or(VaultError::Escape)?;
		fs::create_dir_all(parent)
			.map_err(|source| VaultError::Io { path: parent.to_path_buf(), source })?;
		let temporary = path.with_extension("omp-tmp");
		fs::write(&temporary, bytes)
			.map_err(|source| VaultError::Io { path: temporary.clone(), source })?;
		fs::rename(&temporary, &path).map_err(|source| VaultError::Io { path, source })
	}

	pub fn list(
		&self,
		vault: &str,
		relative: &str,
		limit: usize,
	) -> Result<(Vec<(Str, bool, u64)>, bool), VaultError> {
		let path = if relative.is_empty() {
			self
				.roots
				.read()
				.get(vault)
				.cloned()
				.ok_or_else(|| VaultError::Unknown { name: Str::new(vault) })?
		} else {
			self.target(vault, relative, false)?
		};
		let mut values = Vec::new();
		for item in
			fs::read_dir(&path).map_err(|source| VaultError::Io { path: path.clone(), source })?
		{
			let item = item.map_err(|source| VaultError::Io { path: path.clone(), source })?;
			let metadata = item
				.metadata()
				.map_err(|source| VaultError::Io { path: item.path(), source })?;
			values.push((
				Str::from(item.file_name().to_string_lossy().into_owned()),
				metadata.is_dir(),
				metadata.len(),
			));
		}
		values.sort_by(|a, b| a.0.cmp(&b.0));
		let truncated = values.len() > limit;
		values.truncate(limit);
		Ok((values, truncated))
	}
}

fn parse_vaults(path: &Path) -> Result<BTreeMap<Str, PathBuf>, VaultError> {
	let body = match fs::read_to_string(path) {
		Ok(body) => body,
		Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
		Err(source) => return Err(VaultError::Io { path: path.to_path_buf(), source }),
	};
	let parsed: VaultFile = toml::from_str(&body)
		.map_err(|source| VaultError::Parse { path: path.to_path_buf(), source })?;
	let mut roots = BTreeMap::new();
	for (name, root) in parsed.vaults {
		if name.is_empty() || name.contains(['/', '\\']) {
			return Err(VaultError::InvalidName { name });
		}
		let canonical = root
			.canonicalize()
			.map_err(|source| VaultError::Io { path: root.clone(), source })?;
		if !canonical.is_dir() {
			return Err(VaultError::NotDirectory { path: canonical });
		}
		roots.insert(name, canonical);
	}
	Ok(roots)
}

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
	#[error("cannot access vault path {path}")]
	Io {
		path:   PathBuf,
		#[source]
		source: io::Error,
	},
	#[error("invalid vault configuration {path}")]
	Parse {
		path:   PathBuf,
		#[source]
		source: de::Error,
	},
	#[error("invalid vault name {name}")]
	InvalidName { name: Str },
	#[error("vault root {path} is not a directory")]
	NotDirectory { path: PathBuf },
	#[error("vault {name} is not configured")]
	Unknown { name: Str },
	#[error("vault path escapes its configured root")]
	Escape,
	#[error("vault operation exceeded its {limit}-byte bound")]
	Limit { limit: usize },
}

#[cfg(test)]
mod tests {
	use omp_core::sf;

	use super::*;

	#[test]
	fn layered_service_reads_user_config_root_and_project_shadows_it() {
		let temp = tempfile::tempdir().expect("tempdir");
		let user_root = temp.path().join("o2");
		let project_root = temp.path().join("project");
		let user_notes = temp.path().join("user-notes");
		let project_notes = temp.path().join("project-notes");
		let user_only = temp.path().join("user-only");
		for dir in [&user_root, &project_root.join(".omp"), &user_notes, &project_notes, &user_only] {
			fs::create_dir_all(dir).expect("directory");
		}
		fs::write(user_notes.join("a.md"), "user").expect("user note");
		fs::write(project_notes.join("a.md"), "project").expect("project note");

		let paths = VaultPaths::new(&user_root, &project_root);
		assert_eq!(paths.user, user_root.join("vaults.toml"));
		assert_eq!(paths.project, project_root.join(".omp/vaults.toml"));
		fs::write(
			&paths.user,
			format!(
				"[vaults]\nnotes = {:?}\nextra = {:?}\n",
				user_notes.display().to_string(),
				user_only.display().to_string()
			),
		)
		.expect("user vaults");
		fs::write(
			&paths.project,
			format!("[vaults]\nnotes = {:?}\n", project_notes.display().to_string()),
		)
		.expect("project vaults");

		let service = VaultService::load_layered(&paths).expect("layered load");
		assert_eq!(service.names(), vec![sf!("extra"), sf!("notes")]);
		assert_eq!(service.read("notes", "a.md", 64).expect("shadowed read").as_ref(), b"project");
		assert!(service.list("extra", "", 8).expect("user-only vault").0.is_empty());
		assert!(matches!(service.read("absent", "a.md", 64), Err(VaultError::Unknown { .. })));

		let missing = VaultPaths::new(&temp.path().join("nope"), &temp.path().join("nope"));
		assert!(VaultService::load_layered(&missing).expect("missing files are empty").names().is_empty());
	}
}
