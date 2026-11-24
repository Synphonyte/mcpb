// The README is the crate's documentation, so its example is compiled by
// `cargo test`.
#![doc = include_str!("../README.md")]

mod validate;

use std::collections::BTreeMap;
use std::fs;
use std::io::{ErrorKind, Read, Seek, Write};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

/// The manifest format the bundles are written in.
pub const MANIFEST_VERSION: &str = "0.3";

/// Where the binaries and the files they use go in the bundle.
const SERVER_DIR: &str = "server";

/// Where clients look for the localized strings; the manifest points here.
const RESOURCES_DIR: &str = "mcpb-resources";

/// What the first bytes of a file must be to be a PNG.
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// What can go wrong writing a bundle.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The spec would produce a bundle clients refuse. Every problem is here, and
    /// [`BundleSpec::validate`] reports the same list.
    Invalid(Vec<String>),
    /// A file that belongs in the bundle could not be read.
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The bundle could not be written.
    Write { source: std::io::Error },
    /// A file has no name of its own to be written under, or a name that is not
    /// UTF-8.
    Name(PathBuf),
    /// A file that is meant to be an icon is not a PNG.
    Icon(PathBuf),
    /// Two files of the bundle would be written under the same name.
    Clash { file: PathBuf, name: String },
}

impl Error {
    fn read(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Read {
            path: path.into(),
            source,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(problems) => {
                write!(f, "the bundle can't be packed:")?;
                for problem in problems {
                    write!(f, "\n  - {problem}")?;
                }
                Ok(())
            }
            Self::Read { path, source } => write!(f, "can't read {}: {source}", path.display()),
            Self::Write { source } => write!(f, "can't write the bundle: {source}"),
            Self::Name(path) => write!(
                f,
                "{} needs a UTF-8 file name to be written into the bundle",
                path.display()
            ),
            Self::Icon(path) => write!(
                f,
                "{} is not a PNG: clients only show PNG icons.",
                path.display()
            ),
            Self::Clash { file, name } => write!(
                f,
                "{} would overwrite {name}: two files of the bundle share a name.",
                file.display()
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Invalid(_) | Self::Name(_) | Self::Icon(_) | Self::Clash { .. } => None,
            Self::Read { source, .. } | Self::Write { source } => Some(source),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Self::Write { source }
    }
}

impl From<zip::result::ZipError> for Error {
    fn from(error: zip::result::ZipError) -> Self {
        Self::Write {
            source: std::io::Error::other(error),
        }
    }
}

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
}

/// What kind of value the client asks for in a [`UserConfig`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserConfigKind {
    /// Text the user types.
    String,
    /// A number, which [`UserConfig::min`] and [`UserConfig::max`] bound.
    Number,
    /// A tick box.
    Boolean,
    /// A directory the user picks.
    Directory,
    /// A file the user picks.
    File,
}

impl UserConfigKind {
    /// As the manifest writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Directory => "directory",
            Self::File => "file",
        }
    }
}

/// A value the client asks the user for at install.
///
/// The spec's `args` and `env` refer to it as `${user_config.<key>}`, which the
/// client substitutes with what the user gave.
#[derive(Clone, Debug)]
pub struct UserConfig {
    /// The name `${user_config.<key>}` refers to, such as `project_dir`.
    pub key: String,
    /// What kind of value the user gives.
    pub kind: UserConfigKind,
    /// Shown above the field.
    pub title: String,
    /// Shown under the field.
    pub description: String,
    /// Whether the client refuses to install without a value.
    pub required: bool,
    /// Whether clients hide what the user types, for secrets.
    pub sensitive: bool,
    /// Whether the user may give several values; they are passed as separate
    /// arguments wherever `${user_config.<key>}` stands.
    pub multiple: bool,
    /// What the field starts with.
    pub default: Option<Value>,
    /// The smallest number [`UserConfigKind::Number`] takes.
    pub min: Option<f64>,
    /// The largest number [`UserConfigKind::Number`] takes.
    pub max: Option<f64>,
}

impl UserConfig {
    /// A directory the client asks for, such as the project the server works in.
    pub fn directory(
        key: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self::new(key, UserConfigKind::Directory, title, description)
    }

    /// Text the user types.
    pub fn string(
        key: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self::new(key, UserConfigKind::String, title, description)
    }

    /// A number the user types.
    pub fn number(
        key: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self::new(key, UserConfigKind::Number, title, description)
    }

    /// A tick box.
    pub fn boolean(
        key: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self::new(key, UserConfigKind::Boolean, title, description)
    }

    /// A file the user picks.
    pub fn file(
        key: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self::new(key, UserConfigKind::File, title, description)
    }

    fn new(
        key: impl Into<String>,
        kind: UserConfigKind,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            kind,
            title: title.into(),
            description: description.into(),
            required: false,
            sensitive: false,
            multiple: false,
            default: None,
            min: None,
            max: None,
        }
    }

    fn to_manifest(&self) -> Value {
        let mut config = Map::new();
        config.insert("type".to_string(), json!(self.kind.as_str()));
        config.insert("title".to_string(), json!(self.title));
        config.insert("description".to_string(), json!(self.description));
        for (key, set) in [
            ("required", self.required),
            ("sensitive", self.sensitive),
            ("multiple", self.multiple),
        ] {
            if set {
                config.insert(key.to_string(), json!(true));
            }
        }
        if let Some(default) = &self.default {
            config.insert("default".to_string(), default.clone());
        }
        if let Some(min) = self.min {
            config.insert("min".to_string(), json!(min));
        }
        if let Some(max) = self.max {
            config.insert("max".to_string(), json!(max));
        }
        Value::Object(config)
    }
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

/// What a platform runs instead of the bundle's command, [`BundleSpec::args`]
/// and [`BundleSpec::env`] (`mcp_config.platform_overrides`). Every platform of
/// the bundle starts its own binary; [`BundleSpec::validate`] refuses an
/// override for a platform without one, and an override that sets nothing is
/// not written at all.
#[derive(Clone, Debug, Default)]
pub struct PlatformConfig {
    /// What to run instead of that platform's binary, for a wrapper around it.
    pub command: Option<String>,
    /// What to pass instead of [`BundleSpec::args`].
    pub args: Option<Vec<String>>,
    /// Values added to the environment, replacing [`BundleSpec::env`] wholesale.
    pub env: Option<BTreeMap<String, String>>,
}

impl PlatformConfig {
    fn to_manifest(&self) -> Map<String, Value> {
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
        config
    }
}

/// Who made the server (`author` in the manifest).
#[derive(Clone, Debug, Default)]
pub struct Author {
    /// The person or organisation, as clients show it.
    pub name: String,
    /// Where to reach them.
    pub email: Option<String>,
    /// Their website.
    pub url: Option<String>,
}

impl Author {
    /// An author with a name and nothing else.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            email: None,
            url: None,
        }
    }

    fn to_manifest(&self) -> Value {
        let mut author = json!({ "name": self.name });
        if let Some(email) = &self.email {
            author["email"] = json!(email);
        }
        if let Some(url) = &self.url {
            author["url"] = json!(url);
        }
        author
    }
}

/// Where the server's source lives (`repository` in the manifest).
#[derive(Clone, Debug)]
pub struct Repository {
    /// The kind of repository, such as `git`.
    pub kind: String,
    /// Where it is cloned from.
    pub url: String,
}

impl Repository {
    /// A `git` repository at `url`.
    pub fn git(url: impl Into<String>) -> Self {
        Self {
            kind: "git".to_string(),
            url: url.into(),
        }
    }

    fn to_manifest(&self) -> Value {
        json!({ "type": self.kind, "url": self.url })
    }
}

/// A server binary, and the CPU architecture it was built for.
#[derive(Clone, Debug)]
pub struct Executable {
    /// The compiled server.
    pub path: PathBuf,
    /// Such as `arm64` or `x64`. The manifest cannot switch per architecture,
    /// so it only names the bundle - see [`BundleSpec::file_name`].
    pub arch: String,
}

impl Executable {
    /// The binary at `path`, built for `arch`.
    pub fn new(path: impl Into<PathBuf>, arch: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            arch: arch.into(),
        }
    }
}

/// What goes into a bundle.
///
/// Every field but the name, the version, the description, the author and the
/// executables can be left at [`Default::default`], so a spec is usually built
/// with a struct update:
///
/// ```
/// # use std::collections::BTreeMap;
/// # use mcpb::{Author, BundleSpec, Executable, Platform};
/// let spec = BundleSpec {
///     name: "my-server".into(),
///     version: "1.2.3".into(),
///     description: "What the server does.".into(),
///     author: Author::new("Me"),
///     executables: BTreeMap::from([(
///         Platform::Darwin,
///         Executable::new("target/release/my-server", "arm64"),
///     )]),
///     ..BundleSpec::default()
/// };
/// # let _ = spec;
/// ```
#[derive(Clone, Debug)]
pub struct BundleSpec {
    /// The bundle's and the executable's name, such as `lyte-admin-build-mcp`.
    pub name: String,
    /// What clients show as the extension's name. `name` by default.
    pub display_name: Option<String>,
    /// A semantic version.
    pub version: String,
    /// What the server does, in a sentence.
    pub description: String,
    /// A longer description for extension stores, in markdown.
    pub long_description: Option<String>,
    /// Who made the server.
    pub author: Author,
    /// Where the server's source lives.
    pub repository: Option<Repository>,
    /// The server's website.
    pub homepage: Option<String>,
    /// Where the server is documented.
    pub documentation: Option<String>,
    /// Where users report problems with it.
    pub support: Option<String>,
    /// The licence, as an identifier such as `MIT`.
    pub license: Option<String>,
    /// What the server is about, for stores that search.
    pub keywords: Vec<String>,
    /// The privacy policies of the services the server talks to.
    pub privacy_policies: Vec<String>,
    /// The platforms the bundle runs on, each with the server binary built for
    /// it. One entry is the usual case; several make one bundle that runs on
    /// each of them, each platform starting its own binary.
    pub executables: BTreeMap<Platform, Executable>,
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
    /// What the extension looks like, written to the root of the bundle under
    /// their own file names.
    pub screenshots: Vec<PathBuf>,
    /// Shown to the user before install: name and description of each tool.
    pub tools: Vec<(String, String)>,
    /// Whether the server adds tools of its own at run time.
    pub tools_generated: bool,
    /// Prompts the clients list next to the tools.
    pub prompts: Vec<Prompt>,
    /// Whether the server adds prompts of its own at run time.
    pub prompts_generated: bool,
    /// Translations of [`Self::description`] and the other fields clients show.
    pub localization: Option<Localization>,
    /// What the client asks the user for at install, such as the project the
    /// server works in. `args` and `env` refer to it as `${user_config.<key>}`.
    pub user_config: Vec<UserConfig>,
    /// Arguments for the server. The client substitutes the `${...}` variables
    /// in them.
    pub args: Vec<String>,
    /// Environment variables of the server, which the client adds to its own.
    /// The `${...}` variables in the values are substituted.
    pub env: BTreeMap<String, String>,
    /// What a platform runs instead of the command, [`Self::args`] and
    /// [`Self::env`]. A platform without an executable is refused.
    pub platform_overrides: BTreeMap<Platform, PlatformConfig>,
    /// Platform metadata written verbatim (`_meta`), by reverse-DNS namespace,
    /// each namespace holding an object: such as Windows' `package_family_name`
    /// or a macOS bundle identifier.
    pub meta: Map<String, Value>,
}

impl Default for BundleSpec {
    /// A spec that still has to be filled in: the empty name, version and
    /// description, and no executable, are what [`BundleSpec::validate`]
    /// refuses.
    fn default() -> Self {
        Self {
            name: String::new(),
            display_name: None,
            version: String::new(),
            description: String::new(),
            long_description: None,
            author: Author::default(),
            repository: None,
            homepage: None,
            documentation: None,
            support: None,
            license: None,
            keywords: Vec::new(),
            privacy_policies: Vec::new(),
            executables: BTreeMap::new(),
            extra_executables: Vec::new(),
            resources: Vec::new(),
            icon: None,
            icons: Vec::new(),
            screenshots: Vec::new(),
            tools: Vec::new(),
            tools_generated: false,
            prompts: Vec::new(),
            prompts_generated: false,
            localization: None,
            user_config: Vec::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            platform_overrides: BTreeMap::new(),
            meta: Map::new(),
        }
    }
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
    /// The path of a platform's executable inside the bundle. The first
    /// platform's binary is named after the bundle, so it is the manifest's
    /// `entry_point`; the others carry their platform's name too, since a zip
    /// cannot hold two files with one name.
    fn executable_path(&self, platform: Platform) -> String {
        let stem = if self.executables.keys().next() == Some(&platform) {
            self.name.clone()
        } else {
            format!("{}-{}", self.name, platform.as_str())
        };
        let file = match platform {
            Platform::Win32 => format!("{stem}.exe"),
            _ => stem,
        };
        format!("{SERVER_DIR}/{file}")
    }

    /// The path of the manifest's `entry_point`: the first platform's binary,
    /// which clients start where no platform override says otherwise.
    pub fn entry_point(&self) -> Option<String> {
        self.executables
            .keys()
            .next()
            .map(|platform| self.executable_path(*platform))
    }

    /// The `manifest.json`.
    pub fn manifest(&self) -> Value {
        let mut manifest = json!({
            "manifest_version": MANIFEST_VERSION,
            "name": self.name,
            "display_name": self.display_name.as_deref().unwrap_or(&self.name),
            "version": self.version,
            "description": self.description,
            "author": self.author.to_manifest(),
            "server": {
                "type": "binary",
                "entry_point": self.entry_point().unwrap_or_default(),
                "mcp_config": self.mcp_config(),
            },
            "tools": self
                .tools
                .iter()
                .map(|(name, description)| json!({ "name": name, "description": description }))
                .collect::<Vec<_>>(),
            "compatibility": {
                "platforms": self
                    .executables
                    .keys()
                    .map(|platform| platform.as_str())
                    .collect::<Vec<_>>(),
            },
        });

        for (key, value) in [
            ("long_description", &self.long_description),
            ("homepage", &self.homepage),
            ("documentation", &self.documentation),
            ("support", &self.support),
            ("license", &self.license),
        ] {
            if let Some(value) = value {
                manifest[key] = json!(value);
            }
        }
        if let Some(repository) = &self.repository {
            manifest["repository"] = repository.to_manifest();
        }
        if !self.keywords.is_empty() {
            manifest["keywords"] = json!(self.keywords);
        }
        if !self.privacy_policies.is_empty() {
            manifest["privacy_policies"] = json!(self.privacy_policies);
        }
        if self.tools_generated {
            manifest["tools_generated"] = json!(true);
        }
        if self.prompts_generated {
            manifest["prompts_generated"] = json!(true);
        }
        if !self.meta.is_empty() {
            manifest["_meta"] = Value::Object(self.meta.clone());
        }
        if let Some(icon) = self.manifest_icon() {
            manifest["icon"] = json!(icon);
        }
        if !self.icons.is_empty() {
            manifest["icons"] =
                Value::Array(self.icons.iter().filter_map(Icon::to_manifest).collect());
        }
        if !self.screenshots.is_empty() {
            manifest["screenshots"] = Value::Array(
                self.screenshots
                    .iter()
                    .filter_map(|path| archive_name(path).map(|name| json!(name)))
                    .collect(),
            );
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

        if !self.user_config.is_empty() {
            let mut config = Map::new();
            for entry in &self.user_config {
                config.insert(entry.key.clone(), entry.to_manifest());
            }
            manifest["user_config"] = Value::Object(config);
        }

        manifest
    }

    /// What clients run and with which arguments (`server.mcp_config`).
    fn mcp_config(&self) -> Value {
        let args: Vec<Value> = self.args.iter().map(|arg| json!(arg)).collect();
        let command = self
            .entry_point()
            .map(|path| format!("${{__dirname}}/{path}"));

        let mut config = Map::new();
        config.insert("command".to_string(), json!(command.unwrap_or_default()));
        config.insert("args".to_string(), Value::Array(args));
        config.insert("env".to_string(), json!(self.env));

        // Each platform starts the binary built for it. The first one is the
        // command above; every other platform overrides it, and what the spec
        // overrides itself comes on top.
        let first = self.executables.keys().next().copied();
        let mut overrides = Map::new();
        for platform in self.executables.keys().copied() {
            let mut override_config = self
                .platform_overrides
                .get(&platform)
                .map(PlatformConfig::to_manifest)
                .unwrap_or_default();

            if Some(platform) != first && !override_config.contains_key("command") {
                override_config.insert(
                    "command".to_string(),
                    json!(format!("${{__dirname}}/{}", self.executable_path(platform))),
                );
            }
            if !override_config.is_empty() {
                overrides.insert(
                    platform.as_str().to_string(),
                    Value::Object(override_config),
                );
            }
        }
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

    /// The bundle's file name. For one platform
    /// `<name>-<version>-<platform>-<arch>.mcpb`; for several, each platform
    /// with the architecture given for it, such as
    /// `<name>-<version>-darwin-arm64-win32-x64.mcpb`.
    pub fn file_name(&self) -> String {
        let platforms = self
            .executables
            .iter()
            .map(|(platform, executable)| format!("{}-{}", platform.as_str(), executable.arch))
            .collect::<Vec<_>>()
            .join("-");
        format!("{}-{}-{platforms}.mcpb", self.name, self.version)
    }

    /// Writes the bundle to `path`: `manifest.json`, the server with the
    /// helpers and resources it uses under `server/`, and the icons, screenshots
    /// and translations the manifest points at. Executables are written with
    /// mode 755, everything else with 644.
    ///
    /// A spec that [`Self::validate`] does not accept, and an icon that is not
    /// a PNG, are refused. The bundle is written under a temporary name and
    /// moved into place, so a failed pack leaves no partial bundle behind.
    pub fn pack(&self, path: &Path) -> Result<(), Error> {
        let temp = path.with_extension(format!("{}.tmp", std::process::id()));
        let file = fs::File::create(&temp)?;

        if let Err(error) = self.pack_to(file) {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }

        if let Err(error) = fs::rename(&temp, path) {
            let _ = fs::remove_file(&temp);
            return Err(Error::from(error));
        }

        Ok(())
    }

    /// Writes the bundle to anything that can be written and seeked, such as a
    /// `Cursor` over a `Vec` in memory, and hands it back.
    ///
    /// The same spec packs to the same bytes every time: the entries are written
    /// in a fixed order, with a fixed timestamp and no dependence on the host.
    pub fn pack_to<W: Write + Seek>(&self, writer: W) -> Result<W, Error> {
        if let Err(problems) = self.validate() {
            return Err(Error::Invalid(problems));
        }

        self.check_icons()?;

        let files = self.files()?;
        let manifest =
            serde_json::to_vec_pretty(&self.manifest()).expect("a JSON value serializes");

        let mut zip = zip::ZipWriter::new(writer);

        let plain = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            // A timestamp of its own would make the same spec pack to different
            // bytes on every run.
            .last_modified_time(zip::DateTime::default());

        zip.start_file("manifest.json", plain)?;
        zip.write_all(&manifest)?;

        for (name, file, mode) in files {
            let mut source = fs::File::open(file).map_err(|error| Error::read(file, error))?;
            zip.start_file(name, plain.unix_permissions(mode))?;
            std::io::copy(&mut source, &mut zip)?;
        }

        Ok(zip.finish()?)
    }

    /// Refuses an icon that clients would not show: they only take PNG files.
    fn check_icons(&self) -> Result<(), Error> {
        for icon in self.icon_files() {
            let mut file = fs::File::open(icon).map_err(|error| Error::read(icon, error))?;
            let mut start_of_file = [0; PNG.len()];
            let is_png = match file.read_exact(&mut start_of_file) {
                Ok(()) => start_of_file == *PNG,
                Err(error) if error.kind() == ErrorKind::UnexpectedEof => false,
                Err(error) => return Err(Error::read(icon, error)),
            };
            if !is_png {
                return Err(Error::Icon(icon.to_path_buf()));
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
    fn files(&self) -> Result<Vec<(String, &Path, u32)>, Error> {
        // TODO: single files under `server/` and at the bundle's root only. Nested
        // paths need a directory walk with `.mcpbignore` exclusions. See `TODO.md`.
        const EXECUTABLE: u32 = 0o755;
        const PLAIN: u32 = 0o644;

        let mut files = Vec::new();
        for (platform, executable) in &self.executables {
            files.push((
                self.executable_path(*platform),
                executable.path.as_path(),
                EXECUTABLE,
            ));
        }

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

        // Icons, screenshots and translations keep the places the manifest
        // points at.
        for icon in self.icon_files() {
            files.push((name_of(icon)?.to_string(), icon, PLAIN));
        }
        for screenshot in &self.screenshots {
            files.push((
                name_of(screenshot)?.to_string(),
                screenshot.as_path(),
                PLAIN,
            ));
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
                    return Err(Error::Clash {
                        file: file.to_path_buf(),
                        name,
                    });
                }
                None if name == "manifest.json" => {
                    return Err(Error::Clash {
                        file: file.to_path_buf(),
                        name,
                    });
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
fn name_of(path: &Path) -> Result<&str, Error> {
    archive_name(path).ok_or_else(|| Error::Name(path.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    fn spec(platform: Platform, executable: PathBuf) -> BundleSpec {
        BundleSpec {
            name: "test-server".to_string(),
            version: "1.2.3".to_string(),
            description: "A test server.".to_string(),
            author: Author::new("Test"),
            executables: BTreeMap::from([(platform, Executable::new(executable, "arm64"))]),
            tools: vec![("whoami".to_string(), "Who the caller is.".to_string())],
            user_config: vec![UserConfig::directory(
                "project_dir",
                "Project directory",
                "The directory the server works in.",
            )],
            args: vec!["${user_config.project_dir}".to_string()],
            ..BundleSpec::default()
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

        assert_eq!(
            spec.entry_point().as_deref(),
            Some("server/test-server.exe")
        );
        assert_eq!(spec.file_name(), "test-server-1.2.3-win32-arm64.mcpb");
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
    fn packing_the_same_spec_gives_the_same_bytes() {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = dir.path().join("server-binary");
        fs::write(&binary, b"\x7fELF fake").unwrap();
        let notice = dir.path().join("THIRD-PARTY-NOTICES.md");
        fs::write(&notice, b"notices").unwrap();

        let mut spec = spec(Platform::Linux, binary);
        spec.resources.push(notice);
        spec.icon = Some(png(dir.path(), "icon.png"));

        let in_memory = |spec: &BundleSpec| {
            let mut writer = std::io::Cursor::new(Vec::new());
            writer = spec.pack_to(writer).unwrap();
            writer.into_inner()
        };
        let first = in_memory(&spec);
        assert_eq!(first, in_memory(&spec));

        // Packing to a path writes the same bundle, entry by entry.
        let bundle = dir.path().join("out.mcpb");
        spec.pack(&bundle).unwrap();
        assert_eq!(fs::read(&bundle).unwrap(), first);
    }

    #[test]
    fn a_failed_pack_leaves_no_bundle() {
        let dir = tempfile::TempDir::new().unwrap();
        let spec = spec(Platform::Linux, dir.path().join("missing"));
        let bundle = dir.path().join("out.mcpb");

        let error = spec.pack(&bundle).unwrap_err();
        assert!(error.to_string().contains("can't read"), "{error}");
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
        assert!(error.to_string().contains("share a name"), "{error}");
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

        // The arguments are the spec's own, in its order: the directory the
        // client asks for is passed where the spec puts `${user_config...}`.
        assert_eq!(
            spec.manifest()["server"]["mcp_config"],
            json!({
                "command": "${__dirname}/server/test-server",
                "args": ["${user_config.project_dir}", "--verbose"],
                "env": {
                    "LOG_LEVEL": "debug",
                    "CONFIG": "${__dirname}/server/config.json",
                },
            })
        );
    }

    #[test]
    fn what_the_client_asks_the_user_is_written_as_declared() {
        let mut spec = spec(Platform::Linux, "x".into());
        spec.user_config = vec![
            UserConfig::directory(
                "project_dir",
                "Project directory",
                "The directory the server works in.",
            ),
            UserConfig {
                required: true,
                sensitive: true,
                ..UserConfig::string("api_key", "API key", "The key the server sends.")
            },
            UserConfig {
                min: Some(1.0),
                max: Some(100.0),
                default: Some(json!(10)),
                ..UserConfig::number("limit", "Limit", "How many results.")
            },
            UserConfig {
                default: Some(json!(true)),
                ..UserConfig::boolean("read_only", "Read only", "Never write.")
            },
            UserConfig::file("config_file", "Config file", "The config to load."),
        ];

        let manifest = spec.manifest();
        assert_eq!(
            manifest["user_config"]["project_dir"],
            json!({
                "type": "directory",
                "title": "Project directory",
                "description": "The directory the server works in.",
            })
        );
        assert_eq!(
            manifest["user_config"]["api_key"],
            json!({
                "type": "string",
                "title": "API key",
                "description": "The key the server sends.",
                "required": true,
                "sensitive": true,
            })
        );
        assert_eq!(
            manifest["user_config"]["limit"],
            json!({
                "type": "number",
                "title": "Limit",
                "description": "How many results.",
                "default": 10,
                "min": 1.0,
                "max": 100.0,
            })
        );
        assert_eq!(manifest["user_config"]["read_only"]["type"], "boolean");
        assert_eq!(manifest["user_config"]["config_file"]["type"], "file");

        spec.validate().unwrap();
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
    fn the_manifest_carries_what_stores_show() {
        let mut spec = spec(Platform::Linux, "x".into());
        spec.display_name = Some("Test Server".to_string());
        spec.long_description = Some("A longer text.".to_string());
        spec.author = Author {
            name: "Test".to_string(),
            email: Some("test@example.com".to_string()),
            url: Some("https://example.com".to_string()),
        };
        spec.repository = Some(Repository::git("https://github.com/Synphonyte/mcpb"));
        spec.homepage = Some("https://example.com".to_string());
        spec.documentation = Some("https://docs.example.com".to_string());
        spec.support = Some("https://example.com/issues".to_string());
        spec.license = Some("MIT".to_string());
        spec.keywords = vec!["mcp".to_string()];
        spec.privacy_policies = vec!["https://example.com/privacy".to_string()];
        spec.tools_generated = true;
        spec.prompts_generated = true;
        spec.meta = Map::from_iter([(
            "com.microsoft.windows".to_string(),
            json!({ "package_family_name": "Mcpb" }),
        )]);

        let manifest = spec.manifest();
        assert_eq!(manifest["display_name"], "Test Server");
        assert_eq!(manifest["long_description"], "A longer text.");
        assert_eq!(
            manifest["author"],
            json!({
                "name": "Test",
                "email": "test@example.com",
                "url": "https://example.com",
            })
        );
        assert_eq!(
            manifest["repository"],
            json!({ "type": "git", "url": "https://github.com/Synphonyte/mcpb" })
        );
        assert_eq!(manifest["homepage"], "https://example.com");
        assert_eq!(manifest["documentation"], "https://docs.example.com");
        assert_eq!(manifest["support"], "https://example.com/issues");
        assert_eq!(manifest["license"], "MIT");
        assert_eq!(manifest["keywords"], json!(["mcp"]));
        assert_eq!(
            manifest["privacy_policies"],
            json!(["https://example.com/privacy"])
        );
        assert_eq!(manifest["tools_generated"], true);
        assert_eq!(manifest["prompts_generated"], true);
        assert_eq!(
            manifest["_meta"]["com.microsoft.windows"]["package_family_name"],
            "Mcpb"
        );

        spec.validate().unwrap();
    }

    #[test]
    fn screenshots_are_packed_at_the_root_of_the_bundle() {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = dir.path().join("server-binary");
        fs::write(&binary, b"\x7fELF fake").unwrap();
        let screenshot = dir.path().join("shot.png");
        fs::write(&screenshot, b"whatever a screenshot is").unwrap();

        let mut spec = spec(Platform::Linux, binary);
        spec.screenshots = vec![screenshot];

        assert_eq!(spec.manifest()["screenshots"], json!(["shot.png"]));

        let bundle = dir.path().join("out.mcpb");
        spec.pack(&bundle).unwrap();

        let mut zip = zip::ZipArchive::new(fs::File::open(&bundle).unwrap()).unwrap();
        assert!(zip.by_name("shot.png").is_ok());
    }

    #[test]
    fn a_bundle_of_several_platforms_starts_each_platforms_binary() {
        let dir = tempfile::TempDir::new().unwrap();
        let darwin = dir.path().join("server-darwin");
        fs::write(&darwin, b"\x7fELF fake").unwrap();
        let win32 = dir.path().join("server-win32.exe");
        fs::write(&win32, b"MZ fake").unwrap();

        let mut spec = spec(Platform::Darwin, darwin);
        spec.executables
            .insert(Platform::Win32, Executable::new(win32, "x64"));
        spec.platform_overrides.insert(
            Platform::Win32,
            PlatformConfig {
                args: Some(vec!["--windows".to_string()]),
                ..PlatformConfig::default()
            },
        );

        let manifest = spec.manifest();
        assert_eq!(
            manifest["compatibility"]["platforms"],
            json!(["darwin", "win32"])
        );
        assert_eq!(manifest["server"]["entry_point"], "server/test-server");
        assert_eq!(
            manifest["server"]["mcp_config"]["platform_overrides"]["win32"],
            json!({
                "command": "${__dirname}/server/test-server-win32.exe",
                "args": ["--windows"],
            })
        );
        // The first platform's binary is the command itself.
        assert!(
            manifest["server"]["mcp_config"]["platform_overrides"]
                .get("darwin")
                .is_none()
        );
        assert_eq!(
            spec.file_name(),
            "test-server-1.2.3-darwin-arm64-win32-x64.mcpb"
        );

        let bundle = dir.path().join("out.mcpb");
        spec.pack(&bundle).unwrap();

        let zip = zip::ZipArchive::new(fs::File::open(&bundle).unwrap()).unwrap();
        let mut names: Vec<_> = zip.file_names().map(str::to_string).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "manifest.json",
                "server/test-server",
                "server/test-server-win32.exe",
            ]
        );
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
        assert!(matches!(error, Error::Invalid(_)), "{error}");
        let message = error.to_string();
        assert!(message.contains("can't be packed"), "{message}");
        assert!(message.contains("ASCII letters"), "{message}");
        assert!(message.contains("semantic version"), "{message}");
        assert!(message.contains("${usre_config.project_dir}"), "{message}");
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
            assert!(matches!(error, Error::Icon(_)), "{icon:?}: {error}");
            assert!(error.to_string().contains("not a PNG"), "{icon:?}: {error}");
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
        assert!(
            error.to_string().contains("would overwrite manifest.json"),
            "{error}"
        );
    }
}
