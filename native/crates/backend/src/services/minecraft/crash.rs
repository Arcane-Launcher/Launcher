//! Reads a crashed session's output (console log, crash report, JVM fatal
//! error file) and recognises the common, well-understood causes. Pure text
//! in, findings out — turning findings into advice and fixes for a specific
//! instance happens in `services::instance::crash_help`.
//!
//! The patterns follow the exact wording of Fabric Loader's
//! `Messages.properties`, Forge/NeoForge's mod-sorting errors, Mixin, and
//! HotSpot. Anything not recognised is left for the player to share.

use std::sync::LazyLock;

use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModRef {
    pub id: String,
    pub name: Option<String>,
}

impl ModRef {
    fn new(id: &str, name: Option<&str>) -> Self {
        Self {
            id: id.trim().to_string(),
            name: name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()),
        }
    }

    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// The game ran out of the RAM it was given (`-Xmx`).
    OutOfMemory,
    /// The PC itself ran out of memory, or the allocation is more than
    /// Java could reserve.
    NotEnoughSystemMemory,
    /// A class needs a newer Java than the one running.
    JavaTooOld {
        required: u32,
    },
    /// Old Forge (1.16 and earlier) started on Java 9+.
    JavaTooNewForOldForge,
    MissingMod {
        by: Option<ModRef>,
        dep: ModRef,
    },
    /// A mod requires a different version of another, installed, mod.
    WrongModVersion {
        by: ModRef,
        dep: ModRef,
    },
    /// A mod is for a different Minecraft version.
    WrongMinecraftVersion {
        by: ModRef,
    },
    /// A mod needs a newer mod loader than the instance has.
    LoaderTooOld {
        by: ModRef,
        loader: String,
    },
    Incompatible {
        a: ModRef,
        b: ModRef,
    },
    /// A mod failed while loading (Mixin failure, entrypoint crash,
    /// "has failed to load correctly").
    ModFailed {
        by: ModRef,
    },
    DuplicateMod {
        id: String,
        files: Vec<String>,
    },
    /// The crash report's own "Suspected Mods" line.
    SuspectedMods {
        mods: Vec<ModRef>,
    },
    /// The native graphics driver crashed or has no usable OpenGL.
    GraphicsDriver {
        vendor: Option<GpuVendor>,
    },
    /// A game or mod file is damaged. `file` is the jar named in the error,
    /// when there is one.
    CorruptFile {
        file: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuVendor {
    Amd,
    Nvidia,
    Intel,
}

/// Loader/game ids that appear as "dependencies" in mod resolution errors
/// but aren't mods anyone can install.
const PLATFORM_IDS: &[&str] = &[
    "minecraft",
    "java",
    "fabricloader",
    "quilt_loader",
    "forge",
    "neoforge",
    "fml",
    "javafml",
];

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid crash pattern")
}

/// `'Name' (id)` as Fabric prints a mod. Names can contain apostrophes
/// ("Xaero's Minimap"), so the name ends at the last `' (` before the id.
const FABRIC_MOD: &str = r"'(.+?)' \(([\w.\-]+)\)";

static FABRIC_MISSING: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"(?:Mod )?{FABRIC_MOD} \S+ requires .*? of (?:mod )?(?:{FABRIC_MOD}|([\w.\-]+)), which is missing"
    ))
});
static FABRIC_MISMATCH: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"(?:Mod )?{FABRIC_MOD} \S+ requires (.*?) of (?:mod )?(?:{FABRIC_MOD}|([\w.\-]+)), but only the wrong version"
    ))
});
static FABRIC_BREAKS: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"(?:Mod )?{FABRIC_MOD} \S+ is incompatible with .*? of (?:mod )?(?:{FABRIC_MOD}|([\w.\-]+))"
    ))
});
/// Fabric Loader before 0.12.
static FABRIC_OLD_MISSING: LazyLock<Regex> =
    LazyLock::new(|| re(r"Could not find required mod: ([\w.\-]+) requires \{([\w.\-]+) @"));
static FABRIC_ENTRYPOINT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"Could not execute entrypoint stage '\w+' due to errors, provided by '([\w.\-]+)'")
});

static FORGE_DEP: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"Mod ID: '([\w.\-]+)', Requested by: '([\w.\-]+)', Expected range: '[^']*', Actual version: '([^']+)'",
    )
});
static FORGE_DUPLICATE: LazyLock<Regex> =
    LazyLock::new(|| re(r"Mod ID: '([\w.\-]+)' from mod files: (.+)"));
static FORGE_FAILED: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?:^|\s)([^\s:\]\[][^:\]\[]*?) \(([\w.\-]+)\) (?:has failed to load correctly|encountered an error during the \w+ event phase)",
    )
});

static MIXIN_FAILED: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?:Mixin apply for mod ([\w.\-]+) failed|from mod ([\w.\-]+) failed injection check|Critical injection failure.*?from mod ([\w.\-]+))",
    )
});

static CLASS_VERSION: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"class file version (\d+)(?:\.\d+)?\), this version of the Java Runtime only recognizes class file versions up to (\d+)",
    )
});
static JAVA_NUMBER: LazyLock<Regex> = LazyLock::new(|| re(r"\b(\d{1,2})\b"));

static CORRUPT_JAR: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)(?:zip END header not found|error in opening zip file|Invalid or corrupt jarfile|java\.util\.zip\.ZipException|invalid (?:LOC|CEN) header)[^\n]*",
    )
});
static JAR_NAME: LazyLock<Regex> = LazyLock::new(|| re(r#"([^\s\\/'"]+\.jar)\b"#));

static GPU_DLL: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)\b(atio6axx|atioglxx|atig6pxx|amdxc64|nvoglv64|nvoglv32|ig\d+icd\d+|igxelpicd64)\.dll")
});

/// Reads every recognised cause out of the combined crash text (console
/// log + crash report + JVM error file), most specific first and without
/// duplicates.
pub fn detect(text: &str) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    let push = |f: Finding, out: &mut Vec<Finding>| {
        if !out.contains(&f) {
            out.push(f);
        }
    };

    // --- Mod resolution (the most precise information available) --------
    for c in FABRIC_MISSING.captures_iter(text) {
        let by = ModRef::new(&c[2], Some(&c[1]));
        let dep = match (c.get(3), c.get(4), c.get(5)) {
            (Some(name), Some(id), _) => ModRef::new(id.as_str(), Some(name.as_str())),
            (_, _, Some(id)) => ModRef::new(id.as_str(), None),
            _ => continue,
        };
        if is_platform(&dep.id) {
            continue;
        }
        push(Finding::MissingMod { by: Some(by), dep }, &mut out);
    }
    for c in FABRIC_OLD_MISSING.captures_iter(text) {
        if !is_platform(&c[2]) {
            push(
                Finding::MissingMod {
                    by: Some(ModRef::new(&c[1], None)),
                    dep: ModRef::new(&c[2], None),
                },
                &mut out,
            );
        }
    }
    for c in FABRIC_MISMATCH.captures_iter(text) {
        let by = ModRef::new(&c[2], Some(&c[1]));
        let range = &c[3];
        let dep = match (c.get(4), c.get(5), c.get(6)) {
            (Some(name), Some(id), _) => ModRef::new(id.as_str(), Some(name.as_str())),
            (_, _, Some(id)) => ModRef::new(id.as_str(), None),
            _ => continue,
        };
        push(mismatch_finding(by, dep, range), &mut out);
    }
    for c in FORGE_DEP.captures_iter(text) {
        let dep = ModRef::new(&c[1], None);
        let by = ModRef::new(&c[2], None);
        if &c[3] == "[MISSING]" && !is_platform(&dep.id) {
            push(Finding::MissingMod { by: Some(by), dep }, &mut out);
        } else {
            push(mismatch_finding(by, dep, ""), &mut out);
        }
    }
    for c in FABRIC_BREAKS.captures_iter(text) {
        let a = ModRef::new(&c[2], Some(&c[1]));
        let b = match (c.get(3), c.get(4), c.get(5)) {
            (Some(name), Some(id), _) => ModRef::new(id.as_str(), Some(name.as_str())),
            (_, _, Some(id)) => ModRef::new(id.as_str(), None),
            _ => continue,
        };
        push(Finding::Incompatible { a, b }, &mut out);
    }
    for c in FORGE_DUPLICATE.captures_iter(text) {
        let files: Vec<String> = c[2]
            .split(',')
            .map(|f| {
                f.trim()
                    .trim_matches(|ch| ch == '[' || ch == ']')
                    .to_string()
            })
            .filter(|f| !f.is_empty())
            .collect();
        push(
            Finding::DuplicateMod {
                id: c[1].to_string(),
                files,
            },
            &mut out,
        );
    }

    // --- Java ------------------------------------------------------------
    if let Some(c) = CLASS_VERSION.captures(text) {
        if let Ok(class_version) = c[1].parse::<u32>() {
            push(
                Finding::JavaTooOld {
                    required: class_version.saturating_sub(44),
                },
                &mut out,
            );
        }
    }
    if text.contains("cannot be cast to class java.net.URLClassLoader") {
        push(Finding::JavaTooNewForOldForge, &mut out);
    }

    // --- Memory ----------------------------------------------------------
    if text.contains("java.lang.OutOfMemoryError") {
        push(Finding::OutOfMemory, &mut out);
    }
    if text.contains("There is insufficient memory for the Java Runtime Environment to continue")
        || text.contains("Could not reserve enough space for")
        || text.contains("Invalid maximum heap size")
    {
        push(Finding::NotEnoughSystemMemory, &mut out);
    }

    // --- A specific mod blew up while loading -----------------------------
    for c in MIXIN_FAILED.captures_iter(text) {
        if let Some(id) = c.get(1).or(c.get(2)).or(c.get(3)) {
            push(
                Finding::ModFailed {
                    by: ModRef::new(id.as_str(), None),
                },
                &mut out,
            );
        }
    }
    for c in FABRIC_ENTRYPOINT.captures_iter(text) {
        push(
            Finding::ModFailed {
                by: ModRef::new(&c[1], None),
            },
            &mut out,
        );
    }
    for c in FORGE_FAILED.captures_iter(text) {
        if !is_platform(&c[2]) {
            push(
                Finding::ModFailed {
                    by: ModRef::new(&c[2], Some(&c[1])),
                },
                &mut out,
            );
        }
    }

    // --- Graphics driver -------------------------------------------------
    let gpu_dll = GPU_DLL.captures(text).map(|c| c[1].to_ascii_lowercase());
    let native_crash = text.contains("EXCEPTION_ACCESS_VIOLATION")
        || text.contains("A fatal error has been detected by the Java Runtime");
    let no_opengl = text.contains("Pixel format not accelerated")
        || text.contains("The driver does not appear to support OpenGL")
        || text.contains("GLFW error 65542")
        || text.contains("GLFW error 65543");
    if (native_crash && gpu_dll.is_some()) || no_opengl {
        let vendor = gpu_dll.as_deref().map(|dll| {
            if dll.starts_with("nv") {
                GpuVendor::Nvidia
            } else if dll.starts_with("ig") {
                GpuVendor::Intel
            } else {
                GpuVendor::Amd
            }
        });
        push(Finding::GraphicsDriver { vendor }, &mut out);
    }

    // --- Damaged files ---------------------------------------------------
    if let Some(line) = CORRUPT_JAR.find(text) {
        let file = JAR_NAME
            .captures_iter(line.as_str())
            .last()
            .map(|c| c[1].to_string());
        push(Finding::CorruptFile { file }, &mut out);
    } else if text.contains("Could not find or load main class")
        || text.contains("ClassNotFoundException: net.minecraft.client.main.Main")
    {
        push(Finding::CorruptFile { file: None }, &mut out);
    }

    // --- Last resort: what the crash report itself suspects ---------------
    let has_mod_cause = out.iter().any(|f| {
        matches!(
            f,
            Finding::MissingMod { .. }
                | Finding::WrongModVersion { .. }
                | Finding::WrongMinecraftVersion { .. }
                | Finding::LoaderTooOld { .. }
                | Finding::Incompatible { .. }
                | Finding::ModFailed { .. }
                | Finding::DuplicateMod { .. }
        )
    });
    if !has_mod_cause {
        let mods = suspected_mods(text);
        if !mods.is_empty() {
            push(Finding::SuspectedMods { mods }, &mut out);
        }
    }

    out
}

fn is_platform(id: &str) -> bool {
    PLATFORM_IDS.contains(&id.to_ascii_lowercase().as_str())
}

/// A "requires X of Y, but only the wrong version is present" line, sorted
/// by what Y is.
fn mismatch_finding(by: ModRef, dep: ModRef, range: &str) -> Finding {
    match dep.id.to_ascii_lowercase().as_str() {
        "minecraft" => Finding::WrongMinecraftVersion { by },
        "java" => {
            let required = JAVA_NUMBER
                .captures_iter(range)
                .filter_map(|c| c[1].parse::<u32>().ok())
                .find(|n| *n >= 8)
                .unwrap_or(21);
            Finding::JavaTooOld { required }
        }
        "fabricloader" => Finding::LoaderTooOld {
            by,
            loader: "Fabric Loader".into(),
        },
        "quilt_loader" => Finding::LoaderTooOld {
            by,
            loader: "Quilt Loader".into(),
        },
        "forge" | "fml" | "javafml" => Finding::LoaderTooOld {
            by,
            loader: "Forge".into(),
        },
        "neoforge" => Finding::LoaderTooOld {
            by,
            loader: "NeoForge".into(),
        },
        _ => Finding::WrongModVersion { by, dep },
    }
}

/// The `Suspected Mod(s):` section of a vanilla/Fabric/Forge crash report:
/// either inline (`Suspected Mods: Sodium (sodium), Iris (iris)`) or one per
/// indented line below it. "None"/"Unknown" yields nothing.
fn suspected_mods(text: &str) -> Vec<ModRef> {
    static ENTRY: LazyLock<Regex> = LazyLock::new(|| re(r"([^,\t\n(]+?) \(([\w.\-]+)\)"));
    let mut mods = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed
            .strip_prefix("Suspected Mods:")
            .or_else(|| trimmed.strip_prefix("Suspected Mod:"))
        else {
            continue;
        };
        let mut block = rest.to_string();
        for next in lines.by_ref() {
            if next.trim().is_empty() || !next.starts_with(char::is_whitespace) {
                break;
            }
            block.push('\n');
            block.push_str(next);
        }
        for c in ENTRY.captures_iter(&block) {
            let id = &c[2];
            if is_platform(id) || mods.iter().any(|m: &ModRef| m.id == id) {
                continue;
            }
            mods.push(ModRef::new(id, Some(&c[1])));
        }
        break;
    }
    mods.truncate(3);
    mods
}

/// The crash report's one-line description and the exception that caused
/// it (`Description: Rendering overlay` + the line after it), shown as the
/// technical detail when nothing specific was recognised.
pub fn crash_summary(report: &str) -> Option<String> {
    let mut lines = report.lines();
    let description = lines.find_map(|l| l.strip_prefix("Description:").map(str::trim))?;
    let exception = lines.map(str::trim).find(|l| !l.is_empty());
    Some(match exception {
        Some(e) => format!("{description}\n{e}"),
        None => description.to_string(),
    })
}

/// The crash report path Minecraft prints when its crash handler runs.
pub fn crash_report_path_from_log(log: &str) -> Option<String> {
    static SAVED: LazyLock<Regex> =
        LazyLock::new(|| re(r"Crash report saved to:?\s*(?:#@!@#\s*)?(.+?\.txt)"));
    SAVED
        .captures_iter(log)
        .last()
        .map(|c| c[1].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: &str, name: Option<&str>) -> ModRef {
        ModRef::new(id, name)
    }

    #[test]
    fn fabric_missing_dependency_with_unknown_name() {
        let log = "More details:\n\t - Mod 'Iris' (iris) 1.6.4 requires any version of fabric-api, which is missing!";
        assert_eq!(
            detect(log),
            vec![Finding::MissingMod {
                by: Some(m("iris", Some("Iris"))),
                dep: m("fabric-api", None)
            }]
        );
    }

    #[test]
    fn fabric_missing_dependency_with_name_and_apostrophe() {
        let log = "\t - Mod 'Xaero's World Map' (xaeroworldmap) 1.37 requires version 23.0 or later of mod 'Xaero's Minimap' (xaerominimap), which is missing!";
        assert_eq!(
            detect(log),
            vec![Finding::MissingMod {
                by: Some(m("xaeroworldmap", Some("Xaero's World Map"))),
                dep: m("xaerominimap", Some("Xaero's Minimap"))
            }]
        );
    }

    #[test]
    fn fabric_wrong_minecraft_and_java() {
        let log = "\t - Mod 'Mod Menu' (modmenu) 7.2.2 requires version 1.20.1 of 'Minecraft' (minecraft), but only the wrong version is present: 1.20.4!\n\
                   \t - Mod 'Sodium' (sodium) 0.6.0 requires version 21 or later of 'OpenJDK 64-Bit Server VM' (java), but only the wrong version is present: 17!";
        let found = detect(log);
        assert!(found.contains(&Finding::WrongMinecraftVersion {
            by: m("modmenu", Some("Mod Menu"))
        }));
        assert!(found.contains(&Finding::JavaTooOld { required: 21 }));
    }

    #[test]
    fn fabric_incompatible_pair() {
        let log = "\t - Mod 'Sodium' (sodium) 0.5.3 is incompatible with any version of mod 'OptiFabric' (optifabric), yet a conflicting version is present: 1.13.0!";
        assert_eq!(
            detect(log),
            vec![Finding::Incompatible {
                a: m("sodium", Some("Sodium")),
                b: m("optifabric", Some("OptiFabric"))
            }]
        );
    }

    #[test]
    fn forge_missing_and_version_dependencies() {
        let log = "Missing or unsupported mandatory dependencies:\n\
                   \tMod ID: 'architectury', Requested by: 'rei', Expected range: '[9.1.12,)', Actual version: '[MISSING]'\n\
                   \tMod ID: 'minecraft', Requested by: 'jei', Expected range: '[1.20,1.20.1]', Actual version: '1.20.4'";
        let found = detect(log);
        assert!(found.contains(&Finding::MissingMod {
            by: Some(m("rei", None)),
            dep: m("architectury", None)
        }));
        assert!(found.contains(&Finding::WrongMinecraftVersion { by: m("jei", None) }));
    }

    #[test]
    fn forge_mod_failed_and_duplicates() {
        let log = "Create (create) has failed to load correctly\n\
                   Found duplicate mods:\n\tMod ID: 'jei' from mod files: jei-1.jar, jei-2.jar";
        let found = detect(log);
        assert!(found.contains(&Finding::ModFailed {
            by: m("create", Some("Create"))
        }));
        assert!(found.contains(&Finding::DuplicateMod {
            id: "jei".into(),
            files: vec!["jei-1.jar".into(), "jei-2.jar".into()]
        }));
    }

    #[test]
    fn mixin_failure_names_the_mod() {
        let log = "org.spongepowered.asm.mixin.transformer.throwables.MixinTransformerError: An unexpected critical error was encountered\n\
                   Caused by: org.spongepowered.asm.mixin.throwables.MixinApplyError: Mixin [sodium.mixins.json:core.MixinWindow] from mod sodium failed injection check";
        assert_eq!(
            detect(log),
            vec![Finding::ModFailed {
                by: m("sodium", None)
            }]
        );
    }

    #[test]
    fn memory_and_java_class_version() {
        let log = "java.lang.OutOfMemoryError: Java heap space\n\
                   java.lang.UnsupportedClassVersionError: x has been compiled by a more recent version of the Java Runtime (class file version 65.0), this version of the Java Runtime only recognizes class file versions up to 61.0";
        let found = detect(log);
        assert!(found.contains(&Finding::OutOfMemory));
        assert!(found.contains(&Finding::JavaTooOld { required: 21 }));
    }

    #[test]
    fn graphics_driver_crash_from_problematic_frame() {
        let log = "# A fatal error has been detected by the Java Runtime Environment:\n#  EXCEPTION_ACCESS_VIOLATION (0xc0000005)\n# Problematic frame:\n# C  [atio6axx.dll+0x1a2b3c]";
        assert_eq!(
            detect(log),
            vec![Finding::GraphicsDriver {
                vendor: Some(GpuVendor::Amd)
            }]
        );
    }

    #[test]
    fn suspected_mods_only_without_a_better_cause() {
        let report = "Description: Ticking entity\n\njava.lang.NullPointerException\n\nSuspected Mods: \n\tAlex's Mobs (alexsmobs), Version: 1.22\n\t\tIssue tracker URL: https://x\n\nStacktrace:";
        assert_eq!(
            detect(report),
            vec![Finding::SuspectedMods {
                mods: vec![m("alexsmobs", Some("Alex's Mobs"))]
            }]
        );
        assert_eq!(detect("Suspected Mods: NONE"), vec![]);
        assert_eq!(
            crash_summary(report).as_deref(),
            Some("Ticking entity\njava.lang.NullPointerException")
        );
    }

    #[test]
    fn corrupt_jar_names_the_file() {
        let log = "java.util.zip.ZipException: zip END header not found while reading C:\\mc\\mods\\sodium-0.5.jar";
        assert_eq!(
            detect(log),
            vec![Finding::CorruptFile {
                file: Some("sodium-0.5.jar".into())
            }]
        );
    }

    #[test]
    fn reads_crash_report_path() {
        let log = "#@!@# Game crashed! Crash report saved to: #@!@# C:\\Games\\inst\\crash-reports\\crash-2026-09-25_10.00.00-client.txt";
        assert_eq!(
            crash_report_path_from_log(log).as_deref(),
            Some("C:\\Games\\inst\\crash-reports\\crash-2026-09-25_10.00.00-client.txt")
        );
    }

    #[test]
    fn clean_log_finds_nothing() {
        assert!(detect(
            "[Render thread/INFO]: Stopping!\n[Render thread/INFO]: Sound engine started"
        )
        .is_empty());
    }
}
