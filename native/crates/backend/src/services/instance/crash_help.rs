//! The crash helper: gathers a crashed session's output, recognises the
//! cause (`services::minecraft::crash`) and turns it into plain-language
//! issues with one-click fixes for this particular instance.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::models::common::{InstalledMod, ModLoaderKind, ModProject, ModSource, ProjectType, SearchQuery, SortOrder};
use crate::models::instance::MinecraftInstance;
use crate::services::minecraft::crash::{self, Finding, GpuVendor, ModRef};
use crate::services::minecraft::running::{RunningInstances, SessionEnd};
use crate::services::providers::ProviderRegistry;
use crate::services::system;

use super::jar_ids::{self, ModJar};
use super::mods as instance_mods;

#[derive(Debug, Clone, Serialize)]
pub struct CrashAnalysis {
    /// Whether the last session ended in a crash. `false` also covers "no
    /// session recorded this app run".
    pub crashed: bool,
    pub exit_code: Option<i32>,
    pub issues: Vec<CrashIssue>,
    /// The crash report's description + exception, for the "technical
    /// details" line.
    pub summary: Option<String>,
    pub crash_report_path: Option<String>,
    pub has_log: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CrashIssue {
    pub title: String,
    pub detail: String,
    pub fixes: Vec<CrashFix>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrashFix {
    pub label: String,
    pub action: FixAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FixAction {
    SetMemory { mb: u32 },
    InstallMod { source: ModSource, project_id: String, name: String },
    DisableMod { file_name: String, name: String },
    UseJava { major: u32 },
    Repair,
    OpenUrl { url: String },
}

/// Everything the game left behind for its last session.
pub struct CrashText {
    pub log: String,
    pub report: Option<(PathBuf, String)>,
    pub hs_err: Option<String>,
}

impl CrashText {
    pub fn combined(&self) -> String {
        let mut text = self.log.clone();
        if let Some((_, report)) = &self.report {
            text.push('\n');
            text.push_str(report);
        }
        if let Some(hs_err) = &self.hs_err {
            text.push('\n');
            text.push_str(hs_err);
        }
        text
    }
}

/// Upper bound on what's read from any one file — crash reports and JVM
/// error files are tens of KB; this only guards against something odd.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

async fn read_capped(path: &Path) -> Option<String> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    let bytes = tokio::fs::read(path).await.ok()?;
    let bytes = if meta.len() > MAX_FILE_BYTES {
        bytes[bytes.len() - MAX_FILE_BYTES as usize..].to_vec()
    } else {
        bytes
    };
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The session's console output (or `logs/latest.log` when the launcher
/// was restarted since), the crash report it wrote, and the JVM fatal error
/// file if the JVM itself died.
pub async fn gather(instance: &MinecraftInstance, running: &RunningInstances, end: Option<&SessionEnd>) -> CrashText {
    let game_dir = Path::new(&instance.game_dir);
    let buffered = running.get_log(&instance.id).await;
    let log = if buffered.is_empty() {
        read_capped(&game_dir.join("logs").join("latest.log")).await.unwrap_or_default()
    } else {
        buffered.iter().map(|l| l.line.as_str()).collect::<Vec<_>>().join("\n")
    };

    let mut report_path = crash::crash_report_path_from_log(&log).map(PathBuf::from).filter(|p| p.is_file());
    if report_path.is_none() {
        if let Some(end) = end {
            report_path = newest_crash_report(&game_dir.join("crash-reports"), end.started_at).await;
        }
    }
    let report = match report_path {
        Some(path) => read_capped(&path).await.map(|text| (path, text)),
        None => None,
    };

    let hs_err = match end.and_then(|e| e.pid) {
        Some(pid) => read_capped(&game_dir.join(format!("hs_err_pid{pid}.log"))).await,
        None => None,
    };

    CrashText { log, report, hs_err }
}

async fn newest_crash_report(dir: &Path, since_secs: u64) -> Option<PathBuf> {
    let mut entries = tokio::fs::read_dir(dir).await.ok()?;
    let mut best: Option<(u64, PathBuf)> = None;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("txt") {
            continue;
        }
        let Ok(modified) = entry.metadata().await.and_then(|m| m.modified()) else { continue };
        let secs = modified.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        if secs + 2 >= since_secs && best.as_ref().is_none_or(|(b, _)| secs > *b) {
            best = Some((secs, path));
        }
    }
    best.map(|(_, p)| p)
}

/// What an instance's mods look like on disk, for tying crash findings to
/// files and tracked entries.
struct ModIndex {
    jars: Vec<ModJar>,
    installed: Vec<InstalledMod>,
}

impl ModIndex {
    fn jar(&self, id: &str) -> Option<&ModJar> {
        jar_ids::find_jar(&self.jars, id)
    }

    fn tracked(&self, file_name: &str) -> Option<&InstalledMod> {
        self.installed
            .iter()
            .find(|m| m.enabled && m.project_type == ProjectType::Mod && m.file_name == file_name)
    }

    /// The friendliest name available for a mod the log named.
    fn label(&self, m: &ModRef) -> String {
        if let Some(name) = &m.name {
            return name.clone();
        }
        self.jar(&m.id)
            .and_then(|j| self.tracked(&j.file_name))
            .map(|t| t.name.clone())
            .unwrap_or_else(|| m.id.clone())
    }

    fn disable_fix(&self, m: &ModRef) -> Option<CrashFix> {
        let jar = self.jar(&m.id)?;
        let name = self.label(m);
        Some(CrashFix {
            label: format!("Turn off {name}"),
            action: FixAction::DisableMod { file_name: jar.file_name.clone(), name },
        })
    }

    /// "Update X" for a mod the launcher installed (so it knows where it
    /// came from). Installs the newest version for this Minecraft version
    /// and loader, replacing the current file.
    fn update_fix(&self, m: &ModRef, verb: &str) -> Option<CrashFix> {
        let tracked = self.tracked(&self.jar(&m.id)?.file_name)?;
        Some(CrashFix {
            label: format!("{verb} {}", tracked.name),
            action: FixAction::InstallMod {
                source: tracked.source,
                project_id: tracked.project_id.clone(),
                name: tracked.name.clone(),
            },
        })
    }
}

/// Mod ids whose Modrinth slug is spelled differently. Checked against the
/// live API.
fn modrinth_slugs_for(id: &str) -> Vec<String> {
    let id = jar_ids::normalize_id(id);
    let alias = match id.as_str() {
        "fabric" | "fabric-api-base" => Some("fabric-api"),
        "architectury" => Some("architectury-api"),
        "cloth-config2" => Some("cloth-config"),
        "forgeconfigapiport" => Some("forge-config-api-port"),
        "yet-another-config-lib-v3" | "yet-another-config-lib" | "yacl3" => Some("yacl"),
        "owo" => Some("owo-lib"),
        "puzzleslib" => Some("puzzles-lib"),
        "kotlinforforge" => Some("kotlin-for-forge"),
        "resourcefullib" => Some("resourceful-lib"),
        "bookshelf" => Some("bookshelf-lib"),
        "geckolib3" => Some("geckolib"),
        _ if id.starts_with("cardinal-components") => Some("cardinal-components-api"),
        // Fabric API's modules (fabric-networking-api-v1, fabric-rendering-v1, ...)
        _ if id.starts_with("fabric-") && !id.starts_with("fabric-language-") => Some("fabric-api"),
        _ => None,
    };
    let mut slugs = Vec::new();
    if let Some(a) = alias {
        slugs.push(a.to_string());
    }
    slugs.push(id);
    slugs.dedup();
    slugs
}

fn fits_loader(project: &ModProject, loader: ModLoaderKind) -> bool {
    project.project_type == ProjectType::Mod
        && (project.loaders.iter().any(|l| l == loader.id())
            || (loader == ModLoaderKind::Quilt && project.loaders.iter().any(|l| l == "fabric")))
}

enum Lookup {
    Found(Box<ModProject>),
    NotFound,
    /// Modrinth couldn't be reached, so "not found" would be a guess.
    Unreachable,
}

/// The Modrinth project a missing mod id most likely refers to, if it
/// exists for this instance's loader.
async fn find_modrinth_project(providers: &ProviderRegistry, id: &str, loader: ModLoaderKind) -> Lookup {
    let modrinth = providers.get(ModSource::Modrinth);
    let mut reached = false;
    let answered = |e: &crate::error::AppError| !e.retryable && !e.technical.contains("offline");
    for slug in modrinth_slugs_for(id) {
        match modrinth.get_project(&slug).await {
            Ok(project) => {
                reached = true;
                if fits_loader(&project, loader) {
                    return Lookup::Found(Box::new(project));
                }
            }
            Err(e) => reached |= answered(&e),
        }
    }
    // Fall back to search, accepting only an exact slug/name match so a
    // loosely related mod is never offered.
    let query = SearchQuery {
        query: id.to_string(),
        project_type: ProjectType::Mod,
        loader: Some(loader.id().to_string()),
        game_version: None,
        categories: Vec::new(),
        sort: SortOrder::Relevance,
        offset: 0,
        limit: 5,
        source: ModSource::Modrinth,
    };
    let want = jar_ids::normalize_id(id).replace('-', "");
    match modrinth.search(&query).await {
        Ok(result) => result
            .hits
            .into_iter()
            .find(|p| {
                let slug = p.slug.to_ascii_lowercase().replace(['-', '_'], "");
                let name = p.name.to_ascii_lowercase().replace([' ', '-', '_'], "");
                (slug == want || name == want) && fits_loader(p, loader)
            })
            .map_or(Lookup::NotFound, |p| Lookup::Found(Box::new(p))),
        Err(e) if !reached && !answered(&e) => Lookup::Unreachable,
        Err(_) => Lookup::NotFound,
    }
}

/// "A needs X", "A and B need X", "A, B and 3 more need X".
fn needs_phrase(names: &[String], dep: &str) -> String {
    match names {
        [] => format!("{dep} is missing"),
        [one] => format!("{one} needs {dep}"),
        [a, b] => format!("{a} and {b} need {dep}"),
        [a, b, rest @ ..] => format!("{a}, {b} and {} more need {dep}", rest.len()),
    }
}

/// Missing-mod findings grouped by the Modrinth project they point at
/// (Fabric API's many modules all count as Fabric API): the group's slug,
/// the first missing id seen, and every distinct mod asking for it.
fn group_missing(findings: &[Finding]) -> Vec<(String, ModRef, Vec<ModRef>)> {
    let mut groups: Vec<(String, ModRef, Vec<ModRef>)> = Vec::new();
    for finding in findings {
        let Finding::MissingMod { by, dep } = finding else { continue };
        let key = modrinth_slugs_for(&dep.id).remove(0);
        let group = match groups.iter_mut().find(|(k, _, _)| *k == key) {
            Some(g) => g,
            None => {
                groups.push((key, dep.clone(), Vec::new()));
                groups.last_mut().expect("just pushed")
            }
        };
        if let Some(by) = by {
            if !group.2.iter().any(|m| jar_ids::normalize_id(&m.id) == jar_ids::normalize_id(&by.id)) {
                group.2.push(by.clone());
            }
        }
    }

    groups
}

/// Every "X requires Y, which is missing" line about the same missing mod
/// becomes one issue — a single missing library (Fabric API and its many
/// modules especially) is often reported once per mod that uses it.
async fn missing_mod_issues(
    findings: &[Finding],
    instance: &MinecraftInstance,
    index: &ModIndex,
    providers: &ProviderRegistry,
) -> Vec<CrashIssue> {
    let mut issues = Vec::new();
    for (key, dep, requesters) in group_missing(findings) {
        let lookup = find_modrinth_project(providers, &key, instance.loader).await;
        let dep_label = match &lookup {
            Lookup::Found(p) => p.name.clone(),
            // An aliased group (Fabric API modules) reads better by its slug.
            _ if key != jar_ids::normalize_id(&dep.id) => key.clone(),
            _ => dep.label().to_string(),
        };
        let names: Vec<String> = requesters.iter().map(|r| index.label(r)).collect();
        let detail = match &lookup {
            Lookup::Found(_) => format!("{dep_label} isn't installed in this instance."),
            Lookup::Unreachable => format!(
                "{dep_label} isn't installed, and Arcane couldn't reach Modrinth to find it. Check your \
                 connection, then open this again."
            ),
            Lookup::NotFound => format!(
                "{dep_label} isn't installed, and Arcane couldn't find it on Modrinth for {}. Look for \"{}\" on \
                 the mod's own page or CurseForge.",
                instance.loader.id(),
                dep.id
            ),
        };
        let mut fixes = Vec::new();
        if let Lookup::Found(p) = lookup {
            fixes.push(CrashFix {
                label: format!("Install {}", p.name),
                action: FixAction::InstallMod { source: ModSource::Modrinth, project_id: p.id, name: p.name },
            });
        }
        // Turning a mod off only makes sense when one mod is asking.
        if let [only] = requesters.as_slice() {
            fixes.extend(index.disable_fix(only));
        }
        issues.push(CrashIssue { title: needs_phrase(&names, &dep_label), detail, fixes });
    }
    issues
}

fn round_up_512(mb: u32) -> u32 {
    mb.div_ceil(512) * 512
}

fn gb(mb: u32) -> String {
    let g = mb as f32 / 1024.0;
    if (g - g.round()).abs() < 0.05 {
        format!("{} GB", g.round() as u32)
    } else {
        format!("{g:.1} GB")
    }
}

/// The Java to switch to for "needs Java N": the first long-term-support
/// release at or above it (what Adoptium ships for every platform).
fn lts_java(required: u32) -> u32 {
    [8, 11, 17, 21, 25].into_iter().find(|v| *v >= required).unwrap_or(required)
}

fn driver_url(vendor: GpuVendor) -> &'static str {
    match vendor {
        GpuVendor::Amd => "https://www.amd.com/en/support/download/drivers.html",
        GpuVendor::Nvidia => "https://www.nvidia.com/en-us/drivers/",
        GpuVendor::Intel => "https://www.intel.com/content/www/us/en/support/detect.html",
    }
}

/// Analyses the instance's last session.
pub async fn analyze(
    instance: &MinecraftInstance,
    running: &RunningInstances,
    providers: &ProviderRegistry,
) -> CrashAnalysis {
    let end = running.last_exit(&instance.id).await;
    let text = gather(instance, running, end.as_ref()).await;
    let findings = crash::detect(&text.combined());
    let index = ModIndex {
        jars: jar_ids::scan_mod_jars(instance).await,
        installed: instance_mods::list_installed(instance).await.unwrap_or_default(),
    };

    let crashed = end.as_ref().is_some_and(|e| e.crashed);
    let mut issues = missing_mod_issues(&findings, instance, &index, providers).await;
    for finding in &findings {
        if let Some(issue) = explain(finding, instance, &index) {
            issues.push(issue);
        }
    }

    if issues.is_empty() && crashed {
        issues.push(CrashIssue {
            title: "Minecraft crashed".into(),
            detail: "Arcane couldn't pin down the cause from the log. Share the log in the Arcane Discord and \
                     someone can take a look."
                .into(),
            fixes: Vec::new(),
        });
    }

    CrashAnalysis {
        crashed,
        exit_code: end.and_then(|e| e.code),
        issues,
        summary: text.report.as_ref().and_then(|(_, r)| crash::crash_summary(r)),
        crash_report_path: text.report.map(|(p, _)| p.to_string_lossy().into_owned()),
        has_log: !text.log.trim().is_empty(),
    }
}

fn explain(finding: &Finding, instance: &MinecraftInstance, index: &ModIndex) -> Option<CrashIssue> {
    let issue = |title: String, detail: String, fixes: Vec<Option<CrashFix>>| CrashIssue {
        title,
        detail,
        fixes: fixes.into_iter().flatten().collect(),
    };

    Some(match finding {
        Finding::OutOfMemory => {
            let current = instance.memory_mb.unwrap_or_else(system::default_instance_memory_mb);
            let total = system::total_memory_mb() as u32;
            let cap = (total * 3 / 4) / 512 * 512;
            let target = round_up_512((current + 1024).max(current * 3 / 2)).min(cap);
            if target > current {
                issue(
                    "Minecraft ran out of memory".into(),
                    format!("It was given {} of RAM, which wasn't enough for this game and its mods.", gb(current)),
                    vec![Some(CrashFix {
                        label: format!("Give it {}", gb(target)),
                        action: FixAction::SetMemory { mb: target },
                    })],
                )
            } else {
                issue(
                    "Minecraft ran out of memory".into(),
                    format!(
                        "It already has {} of your PC's {}. Remove some heavy mods or shaders, or close other \
                         programs while playing.",
                        gb(current),
                        gb(total)
                    ),
                    vec![],
                )
            }
        }
        Finding::NotEnoughSystemMemory => {
            let current = instance.memory_mb.unwrap_or_else(system::default_instance_memory_mb);
            let recommended = system::default_instance_memory_mb();
            issue(
                "Your PC couldn't give Minecraft enough memory".into(),
                format!(
                    "Minecraft asked for {} of RAM but Windows couldn't provide it. Close other programs \
                     (browsers use a lot){}.",
                    gb(current),
                    if current > recommended { " or lower the amount" } else { "" }
                ),
                vec![(current > recommended).then(|| CrashFix {
                    label: format!("Lower it to {}", gb(recommended)),
                    action: FixAction::SetMemory { mb: recommended },
                })],
            )
        }
        Finding::JavaTooOld { required } => {
            let major = lts_java(*required);
            issue(
                format!("This needs Java {major}"),
                format!("The game or one of its mods was built for Java {required} or newer, but an older Java ran it."),
                vec![Some(CrashFix { label: format!("Switch to Java {major}"), action: FixAction::UseJava { major } })],
            )
        }
        Finding::JavaTooNewForOldForge => issue(
            "This Forge version needs Java 8".into(),
            "Older Forge (Minecraft 1.16 and earlier) can't start on newer Java.".into(),
            vec![Some(CrashFix { label: "Switch to Java 8".into(), action: FixAction::UseJava { major: 8 } })],
        ),
        // Grouped per missing mod in `missing_mod_issues`.
        Finding::MissingMod { .. } => return None,
        Finding::WrongModVersion { by, dep } => issue(
            format!("{} needs a different version of {}", index.label(by), index.label(dep)),
            format!(
                "The installed {} doesn't match what {} expects. Updating one of them usually fixes it.",
                index.label(dep),
                index.label(by)
            ),
            vec![index.update_fix(dep, "Update"), index.update_fix(by, "Update"), index.disable_fix(by)],
        ),
        Finding::WrongMinecraftVersion { by } => issue(
            format!("{} is made for a different Minecraft version", index.label(by)),
            format!(
                "This instance runs Minecraft {}. Get the version of the mod made for it, or turn the mod off.",
                instance.minecraft_version
            ),
            vec![
                index.update_fix(by, "Update").map(|mut f| {
                    f.label = format!("Get the {} version", instance.minecraft_version);
                    f
                }),
                index.disable_fix(by),
            ],
        ),
        Finding::LoaderTooOld { by, loader } => issue(
            format!("{} needs a newer {loader}", index.label(by)),
            format!(
                "Pick a newer {loader} version with Change version on the instance, or turn this mod off."
            ),
            vec![index.disable_fix(by)],
        ),
        Finding::Incompatible { a, b } => issue(
            format!("{} and {} don't work together", index.label(a), index.label(b)),
            "Their developers marked them as incompatible. Turn one of them off.".into(),
            vec![index.disable_fix(a), index.disable_fix(b)],
        ),
        Finding::ModFailed { by } => issue(
            format!("{} crashed while loading", index.label(by)),
            "It may be broken on this version or clash with another mod. Update it, or turn it off to get \
             playing again."
                .into(),
            vec![index.update_fix(by, "Update"), index.disable_fix(by)],
        ),
        Finding::DuplicateMod { id, files } => {
            let present: Vec<&String> = files.iter().filter(|f| index.jars.iter().any(|j| &&j.file_name == f)).collect();
            issue(
                format!("{id} is installed more than once"),
                "Two copies of the same mod can't load together. Turn off the older one.".into(),
                present
                    .into_iter()
                    .map(|f| {
                        Some(CrashFix {
                            label: format!("Turn off {f}"),
                            action: FixAction::DisableMod { file_name: f.clone(), name: f.clone() },
                        })
                    })
                    .collect(),
            )
        }
        Finding::SuspectedMods { mods } => {
            let names: Vec<String> = mods.iter().map(|m| index.label(m)).collect();
            issue(
                format!("The crash points at {}", names.join(", ")),
                "Minecraft's crash report suspects this. It's a strong hint, not a certainty — try turning it \
                 off first."
                    .into(),
                mods.iter().map(|m| index.disable_fix(m)).collect(),
            )
        }
        Finding::GraphicsDriver { vendor } => issue(
            "Your graphics driver crashed".into(),
            format!(
                "Updating the {}graphics driver usually fixes this. If you use shaders, try without them first.",
                match vendor {
                    Some(GpuVendor::Amd) => "AMD ",
                    Some(GpuVendor::Nvidia) => "NVIDIA ",
                    Some(GpuVendor::Intel) => "Intel ",
                    None => "",
                }
            ),
            vec![vendor.map(|v| CrashFix {
                label: "Get the latest driver".into(),
                action: FixAction::OpenUrl { url: driver_url(v).into() },
            })],
        ),
        Finding::CorruptFile { file } => {
            let jar = file.as_deref().and_then(|f| index.jars.iter().find(|j| j.file_name == f));
            match jar {
                Some(jar) => {
                    let name = index.tracked(&jar.file_name).map(|t| t.name.clone()).unwrap_or(jar.file_name.clone());
                    issue(
                        format!("{name} is damaged"),
                        "The file didn't download completely. Turn it off, then install it again from the Mods tab."
                            .into(),
                        vec![Some(CrashFix {
                            label: format!("Turn off {name}"),
                            action: FixAction::DisableMod { file_name: jar.file_name.clone(), name },
                        })],
                    )
                }
                None => issue(
                    "Some game files are damaged".into(),
                    "A download was probably interrupted. Repairing re-downloads anything missing or broken.".into(),
                    vec![Some(CrashFix { label: "Repair game files".into(), action: FixAction::Repair })],
                ),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_aliases() {
        assert_eq!(modrinth_slugs_for("fabric-networking-api-v1"), vec!["fabric-api", "fabric-networking-api-v1"]);
        assert_eq!(modrinth_slugs_for("fabric-language-kotlin"), vec!["fabric-language-kotlin"]);
        assert_eq!(modrinth_slugs_for("architectury"), vec!["architectury-api", "architectury"]);
        assert_eq!(modrinth_slugs_for("Dynamic_FPS"), vec!["dynamic-fps"]);
    }

    #[test]
    fn groups_missing_mods_by_project() {
        // The four lines Fabric Loader printed for a real test instance with
        // Fabric API turned off (TRansition/TRender are bundled in Entity Culling).
        let log = "	 - Mod 'Dynamic FPS' (dynamic_fps) 3.11.4 requires any version of fabric-lifecycle-events-v1, which is missing!
                   	 - Mod 'EntityCulling' (entityculling) 1.11.2 requires any version of fabric-api, which is missing!
                   	 - Mod 'TRansition' (transition) 1.0.8 requires any version of fabric-api, which is missing!
                   	 - Mod 'TRender' (trender) 1.0.9 requires any version of fabric-api, which is missing!";
        let groups = group_missing(&crate::services::minecraft::crash::detect(log));
        assert_eq!(groups.len(), 1);
        let (key, _, requesters) = &groups[0];
        assert_eq!(key, "fabric-api");
        let names: Vec<&str> = requesters.iter().map(|r| r.label()).collect();
        assert_eq!(names, ["Dynamic FPS", "EntityCulling", "TRansition", "TRender"]);
        let labels: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        assert_eq!(needs_phrase(&labels, "Fabric API"), "Dynamic FPS, EntityCulling and 2 more need Fabric API");
    }

    #[test]
    fn needs_phrases() {
        let n = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(needs_phrase(&n(&["Iris"]), "Fabric API"), "Iris needs Fabric API");
        assert_eq!(needs_phrase(&n(&["A", "B"]), "X"), "A and B need X");
        assert_eq!(needs_phrase(&n(&["A", "B", "C", "D"]), "X"), "A, B and 2 more need X");
    }

    #[test]
    fn java_rounds_up_to_lts() {
        assert_eq!(lts_java(21), 21);
        assert_eq!(lts_java(22), 25);
        assert_eq!(lts_java(16), 17);
    }

    #[test]
    fn memory_formatting() {
        assert_eq!(gb(4096), "4 GB");
        assert_eq!(gb(6656), "6.5 GB");
        assert_eq!(round_up_512(4097), 4608);
    }
}
