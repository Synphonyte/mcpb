// The README is the crate's documentation, so its example is compiled by
// `cargo test`.
#![doc = include_str!("../README.md")]

mod validate;

use std::collections::BTreeMap;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

/// The manifest format the bundles are written in.
pub const MANIFEST_VERSION: &str = "0.3";

/// Where the binaries and the files they use go in the bundle.
const SERVER_DIR: &str = "server";

/// Where clients look for the localized strings; the manifest points here.
const RESOURCES_DIR: &str = "mcpb-resources";

/// The [`BundleSpec::directory`] the manifest declares, which
/// `${user_config.project_dir}` refers to.
pub(crate) const PROJECT_DIR: &str = "project_dir";

/// What the first bytes of a file must be to be a PNG.
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// An operating system a bundle can target (`compatibility.platforms`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Platform {
    Darwin,
    Win32,
    Linux,
}

impl Platform {
    /// As the manifest writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Darwin => "darwin",
            Self::Win32 => "win32",
            Self::Linux => "linux",
        }
    }

    /// From what the manifest, or the name of a bundle, says.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "darwin" | "macos" => Some(Self::Darwin),
            "win32" | "windows" => Some(Self::Win32),
            "linux" => Some(Self::Linux),
            _ => None,
        }
    }

    /// The platform this program was built for.
    pub fn host() -> Self {
        if cfg!(target_os = "macos") {
            Self::Darwin
        } else if cfg!(windows) {
            Self::Win32
        } else {
            Self::Linux
        }
    }

    fn executable_name(self, name: &str) -> String {
        match self {
            Self::Win32 => format!("{name}.exe"),
            _ => name.to_string(),
        }
    }
}

/// A server that asks the user for a directory when the bundle is installed
/// and passes it as its first argument.
#[derive(Clone, Debug)]
pub struct DirectoryPrompt {
    pub title: String,
    pub description: String,
}

/// An icon of the bundle, with the size clients should show it at.
#[derive(Clone, Debug)]
pub struct Icon {
    /// The PNG file, written to the root of the bundle under its own name.
    pub path: PathBuf,
    /// The size it is drawn at, such as `128x128`.
    pub size: String,
    /// `light`, `dark` or a platform's own label. Shown where that theme
    /// applies; without a theme the icon is shown anywhere.
    pub theme: Option<String>,
}

/// A prompt the bundle advertises, so clients can list it before installing.
#[derive(Clone, Debug)]
pub struct Prompt {
    pub name: String,
    /// What the prompt does, shown next to it.
    pub description: Option<String>,
    /// The names [`Self::text`] may use as `${arguments.<name>}`.
    pub arguments: Vec<String>,
    /// The prompt's text, with `${arguments.<name>}` standing in for what the
    /// user answers.
    pub text: String,
}

/// Translated user-facing strings, for clients that read the manifest in the
/// user's language.
#[derive(Clone, Debug)]
pub struct Localization {
    /// The locale whose strings are in the manifest itself, and the one clients
    /// fall back to. A BCP 47 identifier such as `en-US`.
    pub default_locale: String,
    /// The locale ([`Self::default_locale`] or another, such as `de-DE`) and the
    /// JSON file of its overrides, each written to
    /// `mcpb-resources/<locale>.json`.
    pub locales: BTreeMap<String, PathBuf>,
}

/// What a platform runs instead of [`BundleSpec::args`] and
/// [`BundleSpec::env`] (`mcp_config.platform_overrides`). A bundle of this
/// crate runs on [`BundleSpec::platform`] alone, so only that platform's
/// override is read; [`BundleSpec::validate`] refuses any other, and an
/// override that sets nothing is not written at all.
#[derive(Clone, Debug, Default)]
pub struct PlatformConfig {
    /// What to run instead of the server's own command.
    pub command: Option<String>,
    /// What to pass instead of [`BundleSpec::args`].
    pub args: Option<Vec<String>>,
    /// Values added to the environment, replacing [`BundleSpec::env`] wholesale.
    pub env: Option<BTreeMap<String, String>>,
}

impl PlatformConfig {
    fn to_manifest(&self) -> Value {
        let mut config = Map::new();
        if let Some(command) = &self.command {
            config.insert("command".to_string(), json!(command));
        }
        if let Some(args) = &self.args {
            config.insert("args".to_string(), json!(args));
        }
        if let Some(env) = &self.env {
            config.insert("env".to_string(), json!(env));
        }
        Value::Object(config)
    }
}

/// What goes into a bundle.
#[derive(Clone, Debug)]
pub struct BundleSpec {
    /// The bundle's and the executable's name, such as `lyte-admin-build-mcp`.
    pub name: String,
    /// What clients show as the extension's name. `name` by default.
    pub display_name: Option<String>,
    /// A semantic version.
    pub version: String,
    pub description: String,
    pub author: String,
    pub platform: Platform,
    /// The compiled server for [`Self::platform`].
    pub executable: PathBuf,
    /// Further executables placed next to the server in `server/`, under
    /// their own file names (helpers the server runs, such as `labuild`).
    pub extra_executables: Vec<PathBuf>,
    /// Files placed next to the server in `server/` that are read, not run
    /// (notices, licences), under their own file names.
    pub resources: Vec<PathBuf>,
    /// The bundle's icon, a PNG written to the root of the bundle.
    pub icon: Option<PathBuf>,
    /// Further icons, for sizes and themes. The first one without a theme is
    /// also written as the manifest's `icon`, so clients that only read that
    /// field show an icon as well.
    pub icons: Vec<Icon>,
    /// Shown to the user before install: name and description of each tool.
    pub tools: Vec<(String, String)>,
    /// Prompts the clients list next to the tools.
    pub prompts: Vec<Prompt>,
    /// Translations of [`Self::description`] and the other fields clients show.
    pub localization: Option<Localization>,
    /// Ask for a directory at install and pass it as the first argument.
    pub directory: Option<DirectoryPrompt>,
    /// Further arguments, before the directory. The client substitutes the
    /// `${...}` variables in them.
    pub args: Vec<String>,
    /// Environment variables of the server, which the client adds to its own.
    /// The `${...}` variables in the values are substituted.
    pub env: BTreeMap<String, String>,
    /// What [`Self::platform`] runs instead of [`Self::args`] and [`Self::env`].
    pub platform_overrides: BTreeMap<Platform, PlatformConfig>,
}

impl Icon {
    fn to_manifest(&self) -> Option<Value> {
        let mut icon = Map::new();
        icon.insert("src".to_string(), json!(archive_name(&self.path)?));
        icon.insert("size".to_string(), json!(self.size));
        if let Some(theme) = &self.theme {
            icon.insert("theme".to_string(), json!(theme));
        }
        Some(Value::Object(icon))
    }
}

impl Prompt {
    fn to_manifest(&self) -> Value {
        let mut prompt = json!({ "name": self.name, "text": self.text });
        if let Some(description) = &self.description {
            prompt["description"] = json!(description);
        }
        if !self.arguments.is_empty() {
            prompt["arguments"] = json!(self.arguments);
        }
        prompt
    }
}

impl BundleSpec {
    /// The path of the executable inside the bundle.
    pub fn entry_point(&self) -> String {
        format!("{SERVER_DIR}/{}", self.platform.executable_name(&self.name))
    }

    /// The `manifest.json`.
    pub fn manifest(&self) -> Value {
        // TODO: `long_description`, `repository`, `keywords`, `license`,
        // `screenshots`, `tools_generated` and `_meta` are not written yet. See
        // `TODO.md`.
        let mut manifest = json!({
            "manifest_version": MANIFEST_VERSION,
            "name": self.name,
            "display_name": self.display_name.as_deref().unwrap_or(&self.name),
            "version": self.version,
            "description": self.description,
            "author": { "name": self.author },
            "server": {
                "type": "binary",
                "entry_point": self.entry_point(),
                "mcp_config": self.mcp_config(),
            },
            "tools": self
                .tools
                .iter()
                .map(|(name, description)| json!({ "name": name, "description": description }))
                .collect::<Vec<_>>(),
            "compatibility": { "platforms": [self.platform.as_str()] },
        });

        if let Some(icon) = self.manifest_icon() {
            manifest["icon"] = json!(icon);
        }
        if !self.icons.is_empty() {
            manifest["icons"] =
                Value::Array(self.icons.iter().filter_map(Icon::to_manifest).collect());
        }
        if !self.prompts.is_empty() {
            manifest["prompts"] =
                Value::Array(self.prompts.iter().map(Prompt::to_manifest).collect());
        }
        if let Some(localization) = &self.localization {
            manifest["localization"] = json!({
                "resources": format!("{RESOURCES_DIR}/${{locale}}.json"),
                "default_locale": localization.default_locale,
            });
        }

        // TODO: only a directory prompt exists. The spec also has `string` (with
        // `sensitive`), `number`, `boolean` and `file` entries, each with `multiple` and
        // `default`. See `TODO.md`.
        if let Some(directory) = &self.directory {
            manifest["user_config"] = json!({
                PROJECT_DIR: {
                    "type": "directory",
                    "title": directory.title,
                    "description": directory.description,
                    "required": true,
                },
            });
        }

        manifest
    }

    /// What clients run and with which arguments (`server.mcp_config`).
    fn mcp_config(&self) -> Value {
        let mut args: Vec<Value> = self.args.iter().map(|arg| json!(arg)).collect();
        if self.directory.is_some() {
            args.push(json!(format!("${{user_config.{PROJECT_DIR}}}")));
        }

        let mut config = Map::new();
        config.insert(
            "command".to_string(),
            json!(format!("${{__dirname}}/{}", self.entry_point())),
        );
        config.insert("args".to_string(), Value::Array(args));
        config.insert("env".to_string(), json!(self.env));

        let overrides: Map<String, Value> = self
            .platform_overrides
            .iter()
            .map(|(platform, config)| (platform.as_str().to_string(), config.to_manifest()))
            .filter(|(_, config)| config.as_object().is_some_and(|config| !config.is_empty()))
            .collect();
        if !overrides.is_empty() {
            config.insert("platform_overrides".to_string(), Value::Object(overrides));
        }

        Value::Object(config)
    }

    /// The icon the manifest's single `icon` field shows: the one the spec
    /// names, or else the first icon that is not tied to a theme.
    fn manifest_icon(&self) -> Option<&str> {
        self.icon.as_deref().and_then(archive_name).or_else(|| {
            self.icons
                .iter()
                .find(|icon| icon.theme.is_none())
                .and_then(|icon| archive_name(&icon.path))
        })
    }

    /// The bundle's file name: `<name>-<version>-<platform>-<arch>.mcpb`.
    pub fn file_name(&self, arch: &str) -> String {
        format!(
            "{}-{}-{}-{arch}.mcpb",
            self.name,
            self.version,
            self.platform.as_str()
        )
    }

    /// Writes the bundle to `path`: `manifest.json`, the server with the
    /// helpers and resources it uses under `server/`, and the icons and
    /// translations the manifest points at. Executables are written with mode
    /// 755, everything else with 644.
    ///
    /// A spec that [`Self::validate`] does not accept, and an icon that is not
    /// a PNG, are refused before anything is written. The bundle is written
    /// under a temporary name and moved into place, so a failed pack leaves no
    /// partial bundle behind.
    pub fn pack(&self, path: &Path) -> Result<(), String> {
        if let Err(problems) = self.validate() {
            let problems = problems
                .iter()
                .map(|problem| format!("  - {problem}"))
                .collect::<Vec<_>>()
                .join("\n");
            return Err(format!("The bundle can't be packed:\n{problems}"));
        }

        self.check_icons()?;

        let files = self.files()?;
        let manifest =
            serde_json::to_vec_pretty(&self.manifest()).expect("a JSON value serializes");

        let temp = path.with_extension(format!("{}.tmp", std::process::id()));
        let file =
            fs::File::create(&temp).map_err(|error| format!("Can't create {temp:?}: {error}"))?;
        let mut zip = zip::ZipWriter::new(file);

        let entry = |error: zip::result::ZipError| format!("Can't write the bundle: {error}");
        let plain = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        let written = (move || -> Result<(), String> {
            zip.start_file("manifest.json", plain).map_err(entry)?;
            zip.write_all(&manifest)
                .map_err(|error| error.to_string())?;

            for (name, file, mode) in files {
                let bytes =
                    fs::read(file).map_err(|error| format!("Can't read {file:?}: {error}"))?;
                zip.start_file(name, plain.unix_permissions(mode))
                    .map_err(entry)?;
                zip.write_all(&bytes).map_err(|error| error.to_string())?;
            }

            zip.finish().map_err(entry)?;
            Ok(())
        })();

        if let Err(error) = written {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }

        if let Err(error) = fs::rename(&temp, path) {
            let _ = fs::remove_file(&temp);
            return Err(format!("Can't write {path:?}: {error}"));
        }

        Ok(())
    }

    /// Refuses an icon that clients would not show: they only take PNG files.
    fn check_icons(&self) -> Result<(), String> {
        for icon in self.icon_files() {
            let mut file =
                fs::File::open(icon).map_err(|error| format!("Can't read {icon:?}: {error}"))?;
            let mut start_of_file = [0; PNG.len()];
            let is_png = match file.read_exact(&mut start_of_file) {
                Ok(()) => start_of_file == *PNG,
                Err(error) if error.kind() == ErrorKind::UnexpectedEof => false,
                Err(error) => return Err(format!("Can't read {icon:?}: {error}")),
            };
            if !is_png {
                return Err(format!(
                    "{} is not a PNG: clients only show PNG icons.",
                    icon.display()
                ));
            }
        }
        Ok(())
    }

    /// The icons the bundle ships, `icon` first.
    fn icon_files(&self) -> impl Iterator<Item = &Path> {
        self.icon
            .as_deref()
            .into_iter()
            .chain(self.icons.iter().map(|icon| icon.path.as_path()))
    }

    /// Every file of the bundle by archive name, with the mode it is written
    /// with.
    ///
    /// A file used twice - the same icon as `icon` and as an `icons` entry - is
    /// written once. Two different files, or one file with two modes, would
    /// overwrite one another in the zip and are refused.
    fn files(&self) -> Result<Vec<(String, &Path, u32)>, String> {
        // TODO: single files under `server/` and at the bundle's root only. Nested
        // paths need a directory walk with `.mcpbignore` exclusions. See `TODO.md`.
        const EXECUTABLE: u32 = 0o755;
        const PLAIN: u32 = 0o644;

        let mut files = vec![(self.entry_point(), self.executable.as_path(), EXECUTABLE)];

        // Helpers and resources sit next to the server, from where the manifest
        // has clients run them.
        for (paths, mode) in [
            (&self.extra_executables, EXECUTABLE),
            (&self.resources, PLAIN),
        ] {
            for path in paths {
                files.push((
                    format!("{SERVER_DIR}/{}", name_of(path)?),
                    path.as_path(),
                    mode,
                ));
            }
        }

        // Icons and translations keep the places the manifest points at.
        for icon in self.icon_files() {
            files.push((name_of(icon)?.to_string(), icon, PLAIN));
        }
        if let Some(localization) = &self.localization {
            for (locale, path) in &localization.locales {
                files.push((
                    format!("{RESOURCES_DIR}/{locale}.json"),
                    path.as_path(),
                    PLAIN,
                ));
            }
        }

        let mut unique: Vec<(String, &Path, u32)> = Vec::with_capacity(files.len());
        for (name, file, mode) in files {
            let already = unique
                .iter()
                .find(|(taken, _, _)| *taken == name)
                .map(|(_, taken_file, taken_mode)| (*taken_file == file, *taken_mode == mode));

            match already {
                Some((true, true)) => {}
                Some(_) => {
                    return Err(format!(
                        "{} would overwrite {name}: two files of the bundle share a name.",
                        file.display()
                    ));
                }
                None if name == "manifest.json" => {
                    return Err(format!(
                        "{} would overwrite manifest.json: the bundle writes the manifest itself.",
                        file.display()
                    ));
                }
                None => unique.push((name, file, mode)),
            }
        }

        Ok(unique)
    }
}

/// The name a file of the bundle is written under, when its path has one the
/// zip can hold.
fn archive_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(|name| name.to_str())
}

/// The name a file of the bundle must be written under.
fn name_of(path: &Path) -> Result<&str, String> {
    archive_name(path)
        .ok_or_else(|| format!("{path:?} needs a UTF-8 file name to be written into the bundle"))
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    fn spec(platform: Platform, executable: PathBuf) -> BundleSpec {
        BundleSpec {
            name: "test-server".to_string(),
            display_name: None,
            version: "1.2.3".to_string(),
            description: "A test server.".to_string(),
            author: "Test".to_string(),
            platform,
            executable,
            extra_executables: Vec::new(),
            resources: Vec::new(),
            icon: None,
            icons: Vec::new(),
            tools: vec![("whoami".to_string(), "Who the caller is.".to_string())],
            prompts: Vec::new(),
            localization: None,
            directory: Some(DirectoryPrompt {
                title: "Project directory".to_string(),
                description: "The directory the server works in.".to_string(),
            }),
            args: Vec::new(),
            env: BTreeMap::new(),
            platform_overrides: BTreeMap::new(),
        }
    }

    fn png(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let mut bytes = PNG.to_vec();
        bytes.extend_from_slice(b"not really a PNG");
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn the_manifest_has_what_the_spec_requires() {
        let manifest = spec(Platform::Linux, "x".into()).manifest();

        for key in [
            "manifest_version",
            "name",
            "version",
            "description",
            "author",
            "server",
        ] {
            assert!(manifest.get(key).is_some(), "{key}");
        }
        assert_eq!(manifest["author"]["name"], "Test");
        assert_eq!(manifest["server"]["type"], "binary");
        assert_eq!(manifest["server"]["entry_point"], "server/test-server");
        assert_eq!(
            manifest["server"]["mcp_config"]["command"],
            "${__dirname}/server/test-server"
        );
        assert_eq!(
            manifest["server"]["mcp_config"]["args"],
            json!(["${user_config.project_dir}"])
        );
        assert_eq!(manifest["user_config"]["project_dir"]["type"], "directory");
        assert_eq!(manifest["compatibility"]["platforms"], json!(["linux"]));
        assert_eq!(manifest["tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn windows_bundles_hold_an_exe() {
        let spec = spec(Platform::Win32, "x".into());

        assert_eq!(spec.entry_point(), "server/test-server.exe");
        assert_eq!(spec.file_name("x64"), "test-server-1.2.3-win32-x64.mcpb");
    }

    #[test]
    fn a_bundle_holds_the_manifest_and_the_executable() {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = dir.path().join("server-binary");
        fs::write(&binary, b"\x7fELF fake").unwrap();
        let bundle = dir.path().join("out.mcpb");

        spec(Platform::Darwin, binary).pack(&bundle).unwrap();

        let mut zip = zip::ZipArchive::new(fs::File::open(&bundle).unwrap()).unwrap();
        let mut names: Vec<_> = zip.file_names().map(str::to_string).collect();
        names.sort();
        assert_eq!(names, ["manifest.json", "server/test-server"]);

        let mut content = Vec::new();
        zip.by_name("server/test-server")
            .unwrap()
            .read_to_end(&mut content)
            .unwrap();
        assert_eq!(content, b"\x7fELF fake");
        assert_eq!(
            zip.by_name("server/test-server")
                .unwrap()
                .unix_mode()
                .unwrap()
                & 0o111,
            0o111
        );

        let mut manifest = String::new();
        zip.by_name("manifest.json")
            .unwrap()
            .read_to_string(&mut manifest)
            .unwrap();
        let manifest: Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(manifest["compatibility"]["platforms"], json!(["darwin"]));
    }

    #[test]
    fn a_failed_pack_leaves_no_bundle() {
        let dir = tempfile::TempDir::new().unwrap();
        let spec = spec(Platform::Linux, dir.path().join("missing"));
        let bundle = dir.path().join("out.mcpb");

        let error = spec.pack(&bundle).unwrap_err();
        assert!(error.contains("Can't read"), "{error}");
        assert!(!bundle.exists());

        // No temporary file either.
        let files: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(files.is_empty(), "{files:?}");
    }

    #[test]
    fn resources_are_not_executable_and_names_do_not_collide() {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = dir.path().join("server-binary");
        fs::write(&binary, b"\x7fELF fake").unwrap();
        let notice = dir.path().join("THIRD-PARTY-NOTICES.md");
        fs::write(&notice, b"notices").unwrap();

        let mut spec = spec(Platform::Linux, binary);
        spec.resources.push(notice.clone());
        let bundle = dir.path().join("out.mcpb");
        spec.pack(&bundle).unwrap();

        let mut zip = zip::ZipArchive::new(fs::File::open(&bundle).unwrap()).unwrap();
        assert_eq!(
            zip.by_name("server/THIRD-PARTY-NOTICES.md")
                .unwrap()
                .unix_mode()
                .unwrap()
                & 0o111,
            0
        );

        // The same file as an executable would take the same name in the zip.
        spec.extra_executables.push(notice);
        let error = spec.pack(&bundle).unwrap_err();
        assert!(error.contains("share a name"), "{error}");
    }

    #[test]
    fn platforms_are_named_as_the_manifest_does() {
        assert_eq!(Platform::parse("macos"), Some(Platform::Darwin));
        assert_eq!(Platform::parse("windows"), Some(Platform::Win32));
        assert_eq!(Platform::parse("plan9"), None);
    }

    #[test]
    fn the_manifest_says_how_the_server_runs() {
        let mut spec = spec(Platform::Linux, "x".into());
        spec.args.push("--verbose".to_string());
        spec.env
            .insert("LOG_LEVEL".to_string(), "debug".to_string());
        spec.env.insert(
            "CONFIG".to_string(),
            "${__dirname}/server/config.json".to_string(),
        );

        assert_eq!(
            spec.manifest()["server"]["mcp_config"],
            json!({
                "command": "${__dirname}/server/test-server",
                "args": ["--verbose", "${user_config.project_dir}"],
                "env": {
                    "LOG_LEVEL": "debug",
                    "CONFIG": "${__dirname}/server/config.json",
                },
            })
        );
    }

    #[test]
    fn a_platform_runs_what_the_spec_overrides_for_it() {
        let mut overridden = spec(Platform::Win32, "x".into());
        overridden.platform_overrides.insert(
            Platform::Win32,
            PlatformConfig {
                command: Some("${__dirname}/server/test-server.exe".to_string()),
                args: Some(vec!["--windows".to_string()]),
                env: Some(BTreeMap::from([(
                    "TEMP_DIR".to_string(),
                    "${HOME}/temp".to_string(),
                )])),
            },
        );

        assert_eq!(
            overridden.manifest()["server"]["mcp_config"]["platform_overrides"],
            json!({
                "win32": {
                    "command": "${__dirname}/server/test-server.exe",
                    "args": ["--windows"],
                    "env": { "TEMP_DIR": "${HOME}/temp" },
                },
            })
        );

        // An override that sets nothing would be a dead entry in the manifest.
        let mut empty = spec(Platform::Win32, "x".into());
        empty
            .platform_overrides
            .insert(Platform::Win32, PlatformConfig::default());
        assert!(
            empty.manifest()["server"]["mcp_config"]
                .get("platform_overrides")
                .is_none()
        );
    }

    #[test]
    fn prompts_are_advertised_with_their_arguments() {
        let mut spec = spec(Platform::Linux, "x".into());
        spec.prompts.push(Prompt {
            name: "review".to_string(),
            description: Some("Review a file.".to_string()),
            arguments: vec!["path".to_string()],
            text: "Review ${arguments.path}.".to_string(),
        });

        assert_eq!(
            spec.manifest()["prompts"],
            json!([{
                "name": "review",
                "description": "Review a file.",
                "arguments": ["path"],
                "text": "Review ${arguments.path}.",
            }])
        );
    }

    #[test]
    fn icons_are_packed_at_the_root_of_the_bundle() {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = dir.path().join("server-binary");
        fs::write(&binary, b"\x7fELF fake").unwrap();

        let mut spec = spec(Platform::Darwin, binary);
        spec.icon = Some(png(dir.path(), "icon.png"));
        spec.icons = vec![
            Icon {
                path: png(dir.path(), "icon-16-light.png"),
                size: "16x16".to_string(),
                theme: Some("light".to_string()),
            },
            Icon {
                path: png(dir.path(), "icon-512-dark.png"),
                size: "512x512".to_string(),
                theme: Some("dark".to_string()),
            },
        ];

        assert_eq!(spec.manifest()["icon"], "icon.png");
        assert_eq!(
            spec.manifest()["icons"],
            json!([
                { "src": "icon-16-light.png", "size": "16x16", "theme": "light" },
                { "src": "icon-512-dark.png", "size": "512x512", "theme": "dark" },
            ])
        );

        let bundle = dir.path().join("out.mcpb");
        spec.pack(&bundle).unwrap();

        let mut zip = zip::ZipArchive::new(fs::File::open(&bundle).unwrap()).unwrap();
        let mut names: Vec<_> = zip.file_names().map(str::to_string).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "icon-16-light.png",
                "icon-512-dark.png",
                "icon.png",
                "manifest.json",
                "server/test-server",
            ]
        );
        assert_eq!(zip.by_name("icon.png").unwrap().unix_mode(), Some(0o100644));
    }

    #[test]
    fn the_same_icon_twice_is_written_once() {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = dir.path().join("server-binary");
        fs::write(&binary, b"\x7fELF fake").unwrap();
        let icon = png(dir.path(), "icon.png");

        let mut spec = spec(Platform::Linux, binary);
        spec.icon = Some(icon.clone());
        spec.icons = vec![Icon {
            path: icon,
            size: "128x128".to_string(),
            theme: None,
        }];

        let bundle = dir.path().join("out.mcpb");
        spec.pack(&bundle).unwrap();

        assert_eq!(spec.manifest()["icon"], "icon.png");
        assert_eq!(spec.manifest()["icons"][0]["src"], "icon.png");

        let zip = zip::ZipArchive::new(fs::File::open(&bundle).unwrap()).unwrap();
        let icons = zip.file_names().filter(|name| *name == "icon.png").count();
        assert_eq!(icons, 1);
    }

    #[test]
    fn the_first_icon_without_a_theme_is_also_the_plain_icon() {
        let mut spec = spec(Platform::Linux, "x".into());
        spec.icons = vec![
            Icon {
                path: "dark.png".into(),
                size: "16x16".to_string(),
                theme: Some("dark".to_string()),
            },
            Icon {
                path: "plain.png".into(),
                size: "512x512".to_string(),
                theme: None,
            },
        ];

        assert_eq!(spec.manifest()["icon"], "plain.png");
    }

    #[test]
    fn translations_are_packed_where_the_manifest_says() {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = dir.path().join("server-binary");
        fs::write(&binary, b"\x7fELF fake").unwrap();
        let german = dir.path().join("de-DE.json");
        fs::write(&german, br#"{ "description": "Ein Testserver." }"#).unwrap();

        let mut spec = spec(Platform::Linux, binary);
        spec.localization = Some(Localization {
            default_locale: "en-US".to_string(),
            locales: BTreeMap::from([("de-DE".to_string(), german)]),
        });

        assert_eq!(
            spec.manifest()["localization"],
            json!({
                "resources": "mcpb-resources/${locale}.json",
                "default_locale": "en-US",
            })
        );

        let bundle = dir.path().join("out.mcpb");
        spec.pack(&bundle).unwrap();

        let mut zip = zip::ZipArchive::new(fs::File::open(&bundle).unwrap()).unwrap();
        let mut german = String::new();
        zip.by_name("mcpb-resources/de-DE.json")
            .unwrap()
            .read_to_string(&mut german)
            .unwrap();
        assert_eq!(german, r#"{ "description": "Ein Testserver." }"#);
    }

    #[test]
    fn an_invalid_spec_is_refused_before_anything_is_written() {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = dir.path().join("server-binary");
        fs::write(&binary, b"\x7fELF fake").unwrap();

        let mut spec = spec(Platform::Linux, binary);
        spec.name = "my server".to_string();
        spec.version = "1.2".to_string();
        spec.args.push("${usre_config.project_dir}".to_string());

        let bundle = dir.path().join("out.mcpb");
        let error = spec.pack(&bundle).unwrap_err();
        assert!(error.contains("can't be packed"), "{error}");
        assert!(error.contains("ASCII letters"), "{error}");
        assert!(error.contains("semantic version"), "{error}");
        assert!(error.contains("${usre_config.project_dir}"), "{error}");
        assert!(!bundle.exists());
    }

    #[test]
    fn an_icon_that_clients_would_not_show_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = dir.path().join("server-binary");
        fs::write(&binary, b"\x7fELF fake").unwrap();
        let svg = dir.path().join("icon.svg");
        fs::write(&svg, b"<svg></svg>").unwrap();
        // Shorter than the PNG signature, which clients read first.
        let short = dir.path().join("short.png");
        fs::write(&short, &PNG[..4]).unwrap();

        for icon in [svg, short] {
            let mut spec = spec(Platform::Linux, binary.clone());
            spec.icon = Some(icon.clone());

            let bundle = dir.path().join("out.mcpb");
            let error = spec.pack(&bundle).unwrap_err();
            assert!(error.contains("not a PNG"), "{icon:?}: {error}");
            assert!(!bundle.exists());
        }
    }

    #[test]
    fn no_file_of_the_bundle_may_take_the_manifests_name() {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = dir.path().join("server-binary");
        fs::write(&binary, b"\x7fELF fake").unwrap();

        let mut spec = spec(Platform::Linux, binary);
        spec.icon = Some(png(dir.path(), "manifest.json"));

        let error = spec.pack(&dir.path().join("out.mcpb")).unwrap_err();
        assert!(error.contains("would overwrite manifest.json"), "{error}");
    }
}
