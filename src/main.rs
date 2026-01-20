use anyhow::{Context, Result, anyhow, bail};
use cargo_lock::{Lockfile, package::SourceKind};
use clap::Parser;
use serde::Deserialize;
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    fmt::Write,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

// ============================================================================
// CLI Arguments
// ============================================================================

#[derive(Parser, Debug)]
struct Args {
    /// Workspace/Cargo.toml path (optional)
    #[arg(long)]
    manifest_path: Option<PathBuf>,
    /// Root package name (e.g. vector)
    #[arg(long)]
    package: String,
    /// Target triple
    #[arg(long, default_value = "")]
    target: String,
    /// Comma-separated feature list
    #[arg(long, default_value = "")]
    features: String,
    /// Disable default features
    #[arg(long)]
    no_default_features: bool,
    /// Output directory for .inc files
    #[arg(long, default_value = ".")]
    out_dir: PathBuf,
}

// ============================================================================
// Cargo Metadata Types
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct CrateId {
    pub name: String,
    pub version: String,
}

impl CrateId {
    fn new(name: &str, version: &str) -> Self {
        Self {
            name: name.to_string(),
            version: version.to_string(),
        }
    }

    fn from_lock_package(pkg: &cargo_lock::Package) -> Self {
        Self {
            name: pkg.name.as_str().to_string(),
            version: pkg.version.to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    resolve: Option<Resolve>,
    workspace_members: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Package {
    id: String,
    name: String,
    version: String,
    source: Option<String>,
    manifest_path: String,
    license: Option<String>,
}

impl Package {
    fn crate_id(&self) -> CrateId {
        CrateId::new(&self.name, &self.version)
    }

    fn is_git_source(&self) -> bool {
        self.source.as_ref().is_some_and(|s| s.starts_with("git+"))
    }

    fn is_registry_source(&self) -> bool {
        self.source
            .as_ref()
            .is_some_and(|s| s.starts_with("registry+"))
    }
}

#[derive(Debug, Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}

#[derive(Debug, Deserialize)]
struct Node {
    id: String,
    deps: Vec<Dep>,
}


#[derive(Debug, Deserialize)]
struct Dep {
    pkg: String,
    dep_kinds: Vec<DepKind>,
}

impl Dep {
    fn has_normal(&self) -> bool {
        self.dep_kinds.iter().any(|k| k.kind.is_none())
    }
}

#[derive(Debug, Deserialize)]
struct DepKind {
    kind: Option<String>,
}

// ============================================================================
// Output Types
// ============================================================================

struct CrateInfo {
    name: String,
    version: String,
    checksum: String,
}

struct GitRepo {
    url: String,
    rev: String,
    name: String,
}

#[derive(Clone)]
struct GitCrate {
    name: String,
    repo_name: String,
    path_in_repo: String,
}

impl GitCrate {
    fn local_path(&self) -> String {
        if self.path_in_repo.is_empty() {
            format!("../{}", self.repo_name)
        } else {
            format!("../{}/{}", self.repo_name, self.path_in_repo)
        }
    }
}

// ============================================================================
// Main
// ============================================================================

fn main() -> Result<()> {
    let args = Args::parse();

    let meta = cargo_metadata(&args)?;
    let resolve = meta
        .resolve
        .as_ref()
        .ok_or_else(|| anyhow!("metadata missing resolve graph"))?;

    // Maps using full ID strings for traversal
    let pkgs_by_full_id: HashMap<&str, &Package> =
        meta.packages.iter().map(|p| (p.id.as_str(), p)).collect();
    let nodes_by_full_id: HashMap<&str, &Node> =
        resolve.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    // Map using CrateId for output collection
    let pkgs_by_crate_id: HashMap<CrateId, &Package> =
        meta.packages.iter().map(|p| (p.crate_id(), p)).collect();

    let root_id = pick_root_id(&meta, &args.package)?;
    let lock = load_lockfile(args.manifest_path.as_deref())?;
    let checksum_by_id = build_checksum_map(&lock);

    // Compute runtime closure (returns full IDs)
    let runtime_full_ids = compute_runtime_closure(&root_id, &nodes_by_full_id);

    // Convert to CrateIds
    let runtime_ids: BTreeSet<CrateId> = runtime_full_ids
        .iter()
        .filter_map(|id| pkgs_by_full_id.get(id.as_str()).map(|p| p.crate_id()))
        .collect();

    // Get all other crate IDs from the lockfile (everything not in runtime)
    let other_ids: BTreeSet<CrateId> = lock
        .packages
        .iter()
        .map(|p| CrateId::from_lock_package(p))
        .filter(|id| !runtime_ids.contains(id))
        .collect();

    // Collect crates and git repos
    let runtime_crates = collect_registry_crates(&runtime_ids, &pkgs_by_crate_id, &checksum_by_id)?;
    let other_crates = collect_other_registry_crates(&other_ids, &lock)?;
    let runtime_git = collect_git_repos(&runtime_ids, &pkgs_by_crate_id);
    let runtime_git_urls: BTreeSet<_> = runtime_git.iter().map(|r| r.url.as_str()).collect();
    let other_git: Vec<_> = collect_git_repos_from_lock(&other_ids, &lock)
        .into_iter()
        .filter(|r| !runtime_git_urls.contains(r.url.as_str()))
        .collect();

    // Collect git crates for Cargo.toml rewriting
    let runtime_git_crates = collect_git_crates(&runtime_ids, &pkgs_by_crate_id);
    let other_git_crates = collect_git_crates_from_lock(&other_ids, &lock);

    // Create reference vectors for write_inc_file
    let runtime_git_repos: Vec<_> = runtime_git.iter().collect();
    let other_git_repos: Vec<_> = other_git.iter().collect();

    // Write output files
    fs::create_dir_all(&args.out_dir).context("create out_dir")?;

    write_inc_file(
        &args
            .out_dir
            .join(format!("{}-crates-runtime.inc", args.package)),
        &args.package,
        "runtime",
        &runtime_crates,
        &runtime_git,
        Some(&runtime_git_repos),
        Some(&runtime_git_crates),
    )?;

    write_inc_file(
        &args
            .out_dir
            .join(format!("{}-crates-other.inc", args.package)),
        &args.package,
        "other",
        &other_crates,
        &other_git,
        Some(&other_git_repos),
        Some(&other_git_crates),
    )?;

    // Collect and write runtime licenses
    let runtime_licenses = collect_runtime_licenses(&runtime_ids, &pkgs_by_crate_id);
    write_licenses_json(
        &args.out_dir.join(format!("{}-licenses.json", args.package)),
        &runtime_licenses,
    )?;

    Ok(())
}

// ============================================================================
// Cargo Metadata Loading
// ============================================================================

fn cargo_metadata(args: &Args) -> Result<Metadata> {
    let mut cmd = Command::new("cargo");
    cmd.args(["metadata", "--format-version", "1", "--locked"]);

    if let Some(p) = &args.manifest_path {
        cmd.arg("--manifest-path").arg(p);
    }
     if !args.target.trim().is_empty() {
        cmd.arg("--filter-platform").arg(args.target.trim());
    }
    if args.no_default_features {
        cmd.arg("--no-default-features");
    }
    if !args.features.trim().is_empty() {
        cmd.arg("--features").arg(args.features.trim());
    }

    let output = cmd.output().context("run cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    serde_json::from_slice(&output.stdout).context("parse cargo metadata json")
}

fn pick_root_id(meta: &Metadata, name: &str) -> Result<String> {
    let mut candidates: Vec<_> = meta.packages.iter().filter(|p| p.name == name).collect();

    if candidates.is_empty() {
        bail!("no package named {name} in cargo metadata");
    }
    if candidates.len() == 1 {
        return Ok(candidates[0].id.clone());
    }

    // Prefer workspace members if ambiguous
    let ws: BTreeSet<&str> = meta.workspace_members.iter().map(|s| s.as_str()).collect();
    candidates.sort_by_key(|p| (!ws.contains(p.id.as_str()), p.id.as_str()));
    Ok(candidates[0].id.clone())
}

fn load_lockfile(manifest_path: Option<&Path>) -> Result<Lockfile> {
    let start = manifest_path
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or(std::env::current_dir()?);

    for dir in start.ancestors() {
        let lock_path = dir.join("Cargo.lock");
        if lock_path.exists() {
            return Lockfile::load(&lock_path).with_context(|| format!("load {:?}", lock_path));
        }
    }
    bail!("Cargo.lock not found (walked up from {:?})", start)
}

fn build_checksum_map(lock: &Lockfile) -> HashMap<CrateId, String> {
    lock.packages
        .iter()
        .filter_map(|p| {
            p.checksum
                .as_ref()
                .map(|cs| (CrateId::from_lock_package(p), cs.to_string()))
        })
        .collect()
}

// ============================================================================
// Dependency Graph Traversal
// ============================================================================

fn compute_runtime_closure<'a>(
    root_id: &str,
    nodes_by_id: &HashMap<&'a str, &'a Node>,
) -> BTreeSet<String> {
    let mut runtime = BTreeSet::new();
    let mut queue = VecDeque::from([root_id.to_string()]);

    while let Some(id) = queue.pop_front() {
        if !runtime.insert(id.clone()) {
            continue;
        }

        let Some(node) = nodes_by_id.get(id.as_str()) else {
            continue;
        };

        for dep in &node.deps {
            if dep.has_normal() {
                queue.push_back(dep.pkg.clone());
            }
        }
    }

    runtime
}

// ============================================================================
// Crate Collection
// ============================================================================

fn collect_registry_crates(
    ids: &BTreeSet<CrateId>,
    pkgs_by_id: &HashMap<CrateId, &Package>,
    checksum_by_id: &HashMap<CrateId, String>,
) -> Result<Vec<CrateInfo>> {
    let mut crates = Vec::new();

    for id in ids {
        let Some(pkg) = pkgs_by_id.get(id) else {
            continue;
        };

        if pkg.is_git_source() {
            continue;
        }

        if !pkg.is_registry_source() {
            if let Some(src) = &pkg.source {
                eprintln!(
                    "skipping unknown source {} {}: {}",
                    pkg.name, pkg.version, src
                );
            }
            continue;
        }

        let checksum = checksum_by_id.get(id).ok_or_else(|| {
            anyhow!(
                "missing checksum for {} {} (not in Cargo.lock?)",
                pkg.name,
                pkg.version
            )
        })?;

        crates.push(CrateInfo {
            name: pkg.name.clone(),
            version: pkg.version.clone(),
            checksum: checksum.clone(),
        });
    }

    crates.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    crates.dedup_by(|a, b| a.name == b.name && a.version == b.version);
    Ok(crates)
}

/// Collect registry crates from lockfile (for packages not in metadata)
fn collect_other_registry_crates(
    ids: &BTreeSet<CrateId>,
    lock: &Lockfile,
) -> Result<Vec<CrateInfo>> {
    let mut crates = Vec::new();

    for lock_pkg in &lock.packages {
        let crate_id = CrateId::from_lock_package(lock_pkg);
        if !ids.contains(&crate_id) {
            continue;
        }

        let Some(source) = &lock_pkg.source else {
            continue; // path dependency, skip
        };

        // Only include registry crates (not git)
        if !matches!(source.kind(), SourceKind::Registry | SourceKind::SparseRegistry) {
            continue;
        }

        let Some(checksum) = &lock_pkg.checksum else {
            continue;
        };

        crates.push(CrateInfo {
            name: lock_pkg.name.as_str().to_string(),
            version: lock_pkg.version.to_string(),
            checksum: checksum.to_string(),
        });
    }

    crates.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    crates.dedup_by(|a, b| a.name == b.name && a.version == b.version);
    Ok(crates)
}

fn collect_git_repos(ids: &BTreeSet<CrateId>, pkgs_by_id: &HashMap<CrateId, &Package>) -> Vec<GitRepo> {
    let mut seen: HashMap<String, GitRepo> = HashMap::new();

    for id in ids {
        let Some(pkg) = pkgs_by_id.get(id) else {
            continue;
        };

        if let Some(repo) = pkg.source.as_ref().and_then(|s| parse_git_source(s)) {
            seen.entry(repo.url.clone()).or_insert(repo);
        }
    }

    let mut repos: Vec<_> = seen.into_values().collect();
    repos.sort_by(|a, b| a.name.cmp(&b.name));
    repos
}

/// Collect git repos from lockfile packages (for packages not in metadata)
fn collect_git_repos_from_lock(
    ids: &BTreeSet<CrateId>,
    lock: &Lockfile,
) -> Vec<GitRepo> {
    let mut seen: HashMap<String, GitRepo> = HashMap::new();

    for lock_pkg in &lock.packages {
        let crate_id = CrateId::from_lock_package(lock_pkg);
        if !ids.contains(&crate_id) {
            continue;
        }

        let Some(source) = &lock_pkg.source else {
            continue;
        };

        if !matches!(source.kind(), SourceKind::Git(_)) {
            continue;
        }

        let url = source.url().as_str().to_string();
        let rev = source.precise().unwrap_or_default().to_string();
        let repo_name = url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("unknown")
            .trim_end_matches(".git")
            .to_string();

        seen.entry(url.clone()).or_insert(GitRepo {
            url,
            rev,
            name: repo_name,
        });
    }

    let mut repos: Vec<_> = seen.into_values().collect();
    repos.sort_by(|a, b| a.name.cmp(&b.name));
    repos
}

fn collect_git_crates(
    ids: &BTreeSet<CrateId>,
    pkgs_by_id: &HashMap<CrateId, &Package>,
) -> Vec<GitCrate> {
    let mut crates = Vec::new();

    for id in ids {
        let Some(pkg) = pkgs_by_id.get(id) else {
            continue;
        };

        if let Some(repo) = pkg.source.as_ref().and_then(|s| parse_git_source(s)) {
            crates.push(GitCrate {
                name: pkg.name.clone(),
                repo_name: repo.name,
                path_in_repo: extract_path_in_repo(&pkg.manifest_path),
            });
        }
    }

    crates.sort_by(|a, b| a.name.cmp(&b.name));
    crates
}

/// Collect git crates from lockfile (for packages not in metadata)
fn collect_git_crates_from_lock(
    ids: &BTreeSet<CrateId>,
    lock: &Lockfile,
) -> Vec<GitCrate> {
    let mut crates = Vec::new();

    for lock_pkg in &lock.packages {
        let crate_id = CrateId::from_lock_package(lock_pkg);
        if !ids.contains(&crate_id) {
            continue;
        }

        let Some(source) = &lock_pkg.source else {
            continue;
        };

        if !matches!(source.kind(), SourceKind::Git(_)) {
            continue;
        }

        let url = source.url().as_str();
        let repo_name = url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("unknown")
            .trim_end_matches(".git")
            .to_string();

        let crate_name = lock_pkg.name.as_str().to_string();

        // Guess the path in repo: if crate name differs from repo name,
        // assume it's in a subdirectory with the crate name
        let path_in_repo = if crate_name == repo_name {
            String::new()
        } else {
            crate_name.clone()
        };

        crates.push(GitCrate {
            name: crate_name,
            repo_name,
            path_in_repo,
        });
    }

    crates.sort_by(|a, b| a.name.cmp(&b.name));
    crates
}

fn parse_git_source(source: &str) -> Option<GitRepo> {
    let s = source.strip_prefix("git+")?;
    let (url_with_params, rev) = s.rsplit_once('#')?;
    let url = url_with_params.split('?').next()?;
    let repo_name = url
        .trim_end_matches('/')
        .rsplit('/')
        .next()?
        .trim_end_matches(".git");

    Some(GitRepo {
        url: url.to_string(),
        rev: rev.to_string(),
        name: repo_name.to_string(),
    })
}

fn extract_path_in_repo(manifest_path: &str) -> String {
    let parts: Vec<&str> = manifest_path.split('/').collect();

    // Path format: .../checkouts/<repo-hash>/<commit>/<path>/Cargo.toml
    if let Some(idx) = parts.iter().position(|&p| p == "checkouts")
        && parts.len() > idx + 3
    {
        return parts[idx + 3..parts.len() - 1].join("/");
    }
    String::new()
}

// ============================================================================
// Output File Generation
// ============================================================================

fn write_inc_file(
    path: &Path,
    pkg: &str,
    kind: &str,
    crates: &[CrateInfo],
    git_repos: &[GitRepo],
    all_git_for_patch: Option<&[&GitRepo]>,
    git_crates_for_rewrite: Option<&[GitCrate]>,
) -> Result<()> {
    let mut out = String::new();

    writeln!(out, "# Autogenerated for '{pkg}' ({kind})\n").unwrap();

    write_crates_section(&mut out, crates);
    write_git_repos_section(&mut out, git_repos);
    write_do_compile_prepend(&mut out, all_git_for_patch, git_crates_for_rewrite);

    fs::write(path, out).with_context(|| format!("write {:?}", path))
}

fn write_crates_section(out: &mut String, crates: &[CrateInfo]) {
    if crates.is_empty() {
        return;
    }

    out.push_str("SRC_URI += \" \\\n");
    for c in crates {
        writeln!(out, "    crate://crates.io/{}/{} \\", c.name, c.version).unwrap();
    }
    out.push_str("\"\n\n");

    for c in crates {
        writeln!(
            out,
            "SRC_URI[{}-{}.sha256sum] = \"{}\"",
            c.name, c.version, c.checksum
        )
        .unwrap();
    }
    out.push('\n');
}

fn write_git_repos_section(out: &mut String, repos: &[GitRepo]) {
    if repos.is_empty() {
        return;
    }

    out.push_str("SRC_URI += \" \\\n");
    for r in repos {
        let git_url = r.url.replace("https://", "git://");
        writeln!(
            out,
            "    {git_url};protocol=https;nobranch=1;name={};destsuffix={} \\",
            r.name, r.name
        )
        .unwrap();
    }
    out.push_str("\"\n\n");

    for r in repos {
        writeln!(out, "SRCREV_FORMAT .= \"_{}\"", r.name).unwrap();
        writeln!(out, "SRCREV_{} = \"{}\"\n", r.name, r.rev).unwrap();
    }
}

fn write_do_compile_prepend(
    out: &mut String,
    all_git_for_patch: Option<&[&GitRepo]>,
    git_crates_for_rewrite: Option<&[GitCrate]>,
) {
    let has_config_patch = all_git_for_patch.is_some_and(|g| !g.is_empty());
    let has_cargo_rewrite = git_crates_for_rewrite.is_some_and(|c| !c.is_empty());

    if !has_config_patch && !has_cargo_rewrite {
        return;
    }

    out.push_str("do_compile:prepend() {\n");

    if let Some(repos) = all_git_for_patch.filter(|g| !g.is_empty()) {
        write_config_patch_section(out, repos);
    }

    if let Some(crates) = git_crates_for_rewrite.filter(|c| !c.is_empty()) {
        write_cargo_rewrite_section(out, crates);
    }

    out.push_str("}\n");
}

fn write_config_patch_section(out: &mut String, repos: &[&GitRepo]) {
    out.push_str("    cfg=\"${WORKDIR}/cargo_home/config.toml\"\n\n");
    out.push_str("    # Remove autogenerated [patch.\"...\"] blocks that break workspace crates\n");
    out.push_str("    sed -i \\\n");

    for r in repos {
        let escaped_url = r.url.replace('/', "\\/").replace('.', "\\.");
        writeln!(
            out,
            "      -e '/^\\[patch\\.\"{}\"\\]/,/^$/d' \\",
            escaped_url
        )
        .unwrap();
    }
    out.push_str("      \"$cfg\"\n\n");
}

fn write_cargo_rewrite_section(out: &mut String, crates: &[GitCrate]) {
    out.push_str("    # Rewrite git dependencies to local paths for offline build\n");
    out.push_str("    sed -i \\\n");

    for c in crates {
        let escaped_path = c.local_path().replace('/', "\\/");

        // Replace git = "..." with path = "..."
        writeln!(
            out,
            "      -e 's/\\({} = {{[^}}]*\\)git = \"[^\"]*\"/\\1path = \"{}\"/g' \\",
            c.name, escaped_path
        )
        .unwrap();

        // Remove rev/branch/tag attributes
        for attr in ["rev", "branch", "tag"] {
            writeln!(
                out,
                "      -e 's/\\({} = {{[^}}]*\\), *{} = \"[^\"]*\"/\\1/g' \\",
                c.name, attr
            )
            .unwrap();
        }
    }
    out.push_str("      \"${S}/Cargo.toml\"\n");
}

// ============================================================================
// License Collection
// ============================================================================

fn collect_runtime_licenses(
    ids: &BTreeSet<CrateId>,
    pkgs_by_id: &HashMap<CrateId, &Package>,
) -> std::collections::BTreeMap<String, String> {
    let mut licenses = std::collections::BTreeMap::new();

    for id in ids {
        let Some(pkg) = pkgs_by_id.get(id) else {
            continue;
        };

        let license = pkg.license.clone().unwrap_or_else(|| "UNKNOWN".to_string());
        let key = if let Some(source) = &pkg.source {
            if source.starts_with("git+") {
                // Format: git+https://...#rev -> git+https://...@rev
                if let Some((url_part, rev)) = source.rsplit_once('#') {
                    let url = url_part.split('?').next().unwrap_or(url_part);
                    format!("{}@{}", url, rev)
                } else {
                    source.clone()
                }
            } else if source.starts_with("registry+") {
                format!("crate://crates.io/{}/{}", pkg.name, pkg.version)
            } else {
                // Skip unknown sources
                continue;
            }
        } else {
            // Path dependency (workspace member), skip
            continue;
        };

        licenses.insert(key, license);
    }

    licenses
}

fn write_licenses_json(
    path: &Path,
    licenses: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let json = serde_json::to_string_pretty(licenses).context("serialize licenses to JSON")?;
    fs::write(path, json).with_context(|| format!("write {:?}", path))
}
