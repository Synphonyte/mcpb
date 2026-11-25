// The README is the crate docs, so `cargo test` compiles its examples.
#![doc = include_str!("../README.md")]

mod bundle;
mod ignore;
mod manifest;
mod validate;

pub use bundle::{Bundle, Entry};

use std::collections::BTreeMap;
use std::fs;
use std::io::{ErrorKind, Read, Seek, Write};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::ignore::Exclusions;

/// Re-exported for [`BundleSpec::version`], so callers don't need `semver`.
pub use semver::Version;

/// The manifest format the bundles are written in.
pub const MANIFEST_VERSION: &str = "0.3";

/// The bundle directory for the server and its files.
const SERVER_DIR: &str = "server";

/// The bundle directory for translations.
const RESOURCES_DIR: &str = "mcpb-resources";

/// The PNG file signature.
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// What can go wrong packing or reading a bundle.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The spec would produce a bundle clients refuse, with every problem
    /// [`BundleSpec::validate`] reports.
    Invalid(Vec<String>),
    /// A version that is not a semantic version.
    Version(semver::Error),
    /// A file could not be read.
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The bundle could not be written.
    Write { source: std::io::Error },
    /// A file has no name, or a name that is not UTF-8.
    Name(PathBuf),
    /// An icon is not a PNG.
    Icon(PathBuf),
    /// Two different files would get the same name in the bundle.
    Clash { file: PathBuf, name: String },
    /// The file is not a zip with a valid `manifest.json`.
    Bundle { path: PathBuf, reason: String },
    /// An entry's name would escape the directory it is extracted into.
    Unsafe(String),
    /// A manifest field is missing, has the wrong type, or is not supported.
    Manifest(String),
}

impl Error {
    fn read(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Read {
            path: path.into(),
            source,
        }
    }

    fn write(source: std::io::Error) -> Self {
        Self::Write { source }
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
            Self::Version(source) => write!(f, "the version is not a semantic version: {source}"),
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
            Self::Bundle { path, reason } => {
                write!(
                    f,
                    "{} is not a bundle we can read: {reason}",
                    path.display()
                )
            }
            Self::Unsafe(name) => {
                write!(f, "{name} would be written outside the bundle's directory")
            }
            Self::Manifest(reason) => write!(f, "the manifest cannot be read: {reason}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Invalid(_)
            | Self::Name(_)
            | Self::Icon(_)
            | Self::Clash { .. }
            | Self::Bundle { .. }
            | Self::Unsafe(_)
            | Self::Manifest(_) => None,
            Self::Version(source) => Some(source),
            Self::Read { source, .. } | Self::Write { source } => Some(source),
        }
    }
}

impl From<semver::Error> for Error {
    fn from(source: semver::Error) -> Self {
        Self::Version(source)
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
///
/// Platforms sort by their manifest spelling (`darwin`, `linux`, `win32`),
/// which decides the `entry_point` and the bundle's file name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    Darwin,
    Win32,
    Linux,
}

impl Ord for Platform {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl PartialOrd for Platform {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
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

    /// Accepts only the manifest's spelling: `darwin`, `win32` or `linux`.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "darwin" => Some(Self::Darwin),
            "win32" => Some(Self::Win32),
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

/// A value the client asks the user for at install, which `args` and `env`
/// refer to as `${user_config.<key>}`.
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
    /// Whether the user may give several values, passed as separate arguments.
    pub multiple: bool,
    /// The initial value.
    pub default: Option<Value>,
    /// The smallest number [`UserConfigKind::Number`] takes.
    pub min: Option<f64>,
    /// The largest number [`UserConfigKind::Number`] takes.
    pub max: Option<f64>,
}

impl UserConfig {
    /// A directory the user picks.
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

/// An icon and the size it is shown at.
#[derive(Clone, Debug)]
pub struct Icon {
    /// The PNG file. It goes to the bundle's root unless a
    /// [`BundleSpec::files`] entry with the same path places it.
    pub path: PathBuf,
    /// Such as `128x128`.
    pub size: String,
    /// `light`, `dark` or a platform-specific label; `None` for any theme.
    pub theme: Option<String>,
}

/// A prompt the bundle advertises, listed before install.
#[derive(Clone, Debug)]
pub struct Prompt {
    pub name: String,
    /// What the prompt does, shown next to it.
    pub description: Option<String>,
    /// The names [`Self::text`] may use as `${arguments.<name>}`.
    pub arguments: Vec<String>,
    /// The prompt's text, where `${arguments.<name>}` is replaced by the argument.
    pub text: String,
}

/// Translations of the manifest's user-facing strings.
#[derive(Clone, Debug)]
pub struct Localization {
    /// The locale of the manifest's own strings and the fallback, such as `en-US`.
    pub default_locale: String,
    /// Each locale's JSON file of overrides, written to
    /// `mcpb-resources/<locale>.json`.
    pub locales: BTreeMap<String, PathBuf>,
}

/// A platform's replacement for the bundle's command, [`BundleSpec::args`] and
/// [`BundleSpec::env`] (`mcp_config.platform_overrides`). Only platforms with
/// an executable may have one.
#[derive(Clone, Debug, Default)]
pub struct PlatformConfig {
    /// A wrapper to run instead of the platform's binary.
    pub command: Option<String>,
    /// What to pass instead of [`BundleSpec::args`].
    pub args: Option<Vec<String>>,
    /// Environment variables to set instead of [`BundleSpec::env`].
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
    /// The person or organisation.
    pub name: String,
    /// Their email address.
    pub email: Option<String>,
    /// Their website.
    pub url: Option<String>,
}

impl Author {
    /// An author with only a name.
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
    /// Such as `git`.
    pub kind: String,
    /// The clone URL.
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

/// A server binary and the CPU architecture it was built for.
#[derive(Clone, Debug)]
pub struct Executable {
    /// The compiled binary.
    pub path: PathBuf,
    /// Such as `arm64` or `x64`. Only used in [`BundleSpec::file_name`], since
    /// the manifest doesn't record it; `None` when read from a manifest.
    pub arch: Option<String>,
    /// Its path below `server/`, such as `bin/my-server`, without Windows'
    /// `.exe`. `None` names it after the bundle.
    pub name: Option<String>,
}

impl Executable {
    /// The binary at `path`, built for `arch`.
    pub fn new(path: impl Into<PathBuf>, arch: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            arch: Some(arch.into()),
            name: None,
        }
    }

    /// Writes the binary as `server/<name>` instead of after the bundle.
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// A single file placed at a path of its own, such as
/// `server/lib/libcalculator.dylib`.
#[derive(Clone, Debug)]
pub struct BundleFile {
    /// The file on disk.
    pub path: PathBuf,
    /// Its path in the bundle, with `/` separators.
    pub name: String,
    /// 755 for executables, 644 for plain files.
    pub mode: u32,
}

impl BundleFile {
    /// The file at `path`, written as executable `name`.
    pub fn new(path: impl Into<PathBuf>, name: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            name: name.into(),
            mode: 0o755,
        }
    }

    /// The same, as a plain file.
    pub fn data(path: impl Into<PathBuf>, name: impl Into<String>) -> Self {
        Self {
            mode: 0o644,
            ..Self::new(path, name)
        }
    }
}

/// A directory packed as it is laid out on disk, such as data files or packages.
///
/// It skips the format's own exclusions (`.git/`, `*.log`,
/// `node_modules/.cache/`, lockfiles and the like), the `.mcpbignore` itself,
/// and the patterns of a `.mcpbignore` at its root, read as a `.gitignore` is.
#[derive(Clone, Debug)]
pub struct Tree {
    /// The directory to pack.
    pub path: PathBuf,
    /// Its directory in the bundle, such as `server`, or `""` for the root.
    pub into: String,
    /// 755 for executables, 644 for plain files.
    pub mode: u32,
}

impl Tree {
    /// The directory's files, as executables in `server/`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            into: SERVER_DIR.to_string(),
            mode: 0o755,
        }
    }

    /// The same, as plain files.
    pub fn data(path: impl Into<PathBuf>) -> Self {
        Self {
            mode: 0o644,
            ..Self::new(path)
        }
    }

    /// Packs into `into` instead, such as `server/assets`, or `""` for the root.
    pub fn in_dir(mut self, into: impl Into<String>) -> Self {
        self.into = into.into();
        self
    }
}

/// What goes into a bundle.
///
/// Every field but the name, version, description, author and executables can
/// be left at its default:
///
/// ```
/// # use std::collections::BTreeMap;
/// # use mcpb::{Author, BundleSpec, Executable, Platform};
/// let spec = BundleSpec {
///     name: "my-server".into(),
///     version: "1.2.3".parse().expect("a semantic version"),
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
    /// Names the bundle and its executable, such as `my-server`.
    pub name: String,
    /// The name clients show; `name` by default.
    pub display_name: Option<String>,
    /// The version, also used in the bundle's file name.
    pub version: Version,
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
    /// Where users report problems.
    pub support: Option<String>,
    /// An SPDX identifier such as `MIT`.
    pub license: Option<String>,
    /// Search keywords for stores.
    pub keywords: Vec<String>,
    /// The privacy policies of the services the server talks to.
    pub privacy_policies: Vec<String>,
    /// The server binary for each platform the bundle runs on.
    pub executables: BTreeMap<Platform, Executable>,
    /// Helper executables, written to `server/` under their own file names.
    pub extra_executables: Vec<PathBuf>,
    /// Plain files such as notices, written to `server/` under their own names.
    pub resources: Vec<PathBuf>,
    /// Directories packed whole; see [`Tree`].
    pub trees: Vec<Tree>,
    /// Single files placed at a path of their own; see [`BundleFile`].
    pub files: Vec<BundleFile>,
    /// The bundle's PNG icon. It goes to the bundle's root unless one of
    /// [`Self::files`] with the same path places it.
    pub icon: Option<PathBuf>,
    /// Icons for other sizes and themes, placed like [`Self::icon`]. Without an
    /// `icon`, the first one without a theme is also written as the manifest's
    /// `icon`.
    pub icons: Vec<Icon>,
    /// Screenshots, placed like [`Self::icon`].
    pub screenshots: Vec<PathBuf>,
    /// The name and description of each tool, shown before install.
    pub tools: Vec<(String, String)>,
    /// Whether the server adds tools of its own at run time.
    pub tools_generated: bool,
    /// Prompts, listed next to the tools.
    pub prompts: Vec<Prompt>,
    /// Whether the server adds prompts of its own at run time.
    pub prompts_generated: bool,
    /// Translations of [`Self::description`] and the other fields clients show.
    pub localization: Option<Localization>,
    /// What the client asks the user for at install; see [`UserConfig`].
    pub user_config: Vec<UserConfig>,
    /// The server's arguments, with `${...}` variables substituted by the client.
    pub args: Vec<String>,
    /// The server's environment variables, with `${...}` variables substituted.
    pub env: BTreeMap<String, String>,
    /// Per-platform replacements; see [`PlatformConfig`].
    pub platform_overrides: BTreeMap<Platform, PlatformConfig>,
    /// Client-specific metadata (`_meta`), written verbatim: an object per
    /// reverse-DNS namespace.
    pub meta: Map<String, Value>,
}

impl Default for BundleSpec {
    /// An empty spec; the required fields still have to be filled in.
    fn default() -> Self {
        Self {
            name: String::new(),
            display_name: None,
            version: Version::new(0, 0, 0),
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
            trees: Vec::new(),
            files: Vec::new(),
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
    /// The manifest entry for the icon, stored in the bundle as `name`.
    fn to_manifest(&self, name: &str) -> Value {
        let mut icon = Map::new();
        icon.insert("src".to_string(), json!(name));
        icon.insert("size".to_string(), json!(self.size));
        if let Some(theme) = &self.theme {
            icon.insert("theme".to_string(), json!(theme));
        }
        Value::Object(icon)
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
    /// The path of a platform's binary in the bundle: its own name, else
    /// `server/<name>` for the first platform and `server/<name>-<platform>` for
    /// the others.
    pub(crate) fn executable_path(&self, platform: Platform) -> String {
        let named = self
            .executables
            .get(&platform)
            .and_then(|executable| executable.name.clone());
        let file = named.unwrap_or_else(|| {
            if self.executables.keys().next() == Some(&platform) {
                self.name.clone()
            } else {
                format!("{}-{}", self.name, platform.as_str())
            }
        });
        let file = match platform {
            Platform::Win32 if !file.ends_with(".exe") => format!("{file}.exe"),
            _ => file,
        };
        format!("{SERVER_DIR}/{file}")
    }

    /// The manifest's `entry_point`: the first platform's binary.
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
            "version": self.version.to_string(),
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
            manifest["icons"] = Value::Array(
                self.icons
                    .iter()
                    .filter_map(|icon| {
                        self.bundle_name(&icon.path)
                            .map(|name| icon.to_manifest(name))
                    })
                    .collect(),
            );
        }
        if !self.screenshots.is_empty() {
            manifest["screenshots"] = Value::Array(
                self.screenshots
                    .iter()
                    .filter_map(|path| self.bundle_name(path).map(|name| json!(name)))
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

        // Every other platform overrides the command with its own binary, unless
        // the spec gives it a command of its own.
        let entry = self.entry_point();
        let mut overrides = Map::new();
        for platform in self.executables.keys().copied() {
            let mut override_config = self
                .platform_overrides
                .get(&platform)
                .map(PlatformConfig::to_manifest)
                .unwrap_or_default();

            let own_binary = Some(self.executable_path(platform));
            if entry != own_binary && !override_config.contains_key("command") {
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

    /// An icon's or screenshot's name in the bundle: where a [`BundleFile`]
    /// places it, else its file name at the root.
    fn bundle_name<'a>(&'a self, path: &'a Path) -> Option<&'a str> {
        match self.placed(path) {
            Some(file) => Some(&file.name),
            None => archive_name(path),
        }
    }

    /// The [`BundleFile`] for `path`, if any.
    fn placed(&self, path: &Path) -> Option<&BundleFile> {
        self.files.iter().find(|file| file.path == path)
    }

    /// The manifest's `icon`: [`Self::icon`], else the first unthemed icon.
    fn manifest_icon(&self) -> Option<&str> {
        self.icon
            .as_deref()
            .and_then(|path| self.bundle_name(path))
            .or_else(|| {
                self.icons
                    .iter()
                    .find(|icon| icon.theme.is_none())
                    .and_then(|icon| self.bundle_name(&icon.path))
            })
    }

    /// The bundle's file name, such as `<name>-<version>-darwin-arm64.mcpb`, with
    /// each platform and its architecture, if known.
    pub fn file_name(&self) -> String {
        let platforms = self
            .executables
            .iter()
            .map(|(platform, executable)| match &executable.arch {
                Some(arch) => format!("{}-{arch}", platform.as_str()),
                None => platform.as_str().to_string(),
            })
            .collect::<Vec<_>>()
            .join("-");
        format!("{}-{}-{platforms}.mcpb", self.name, self.version)
    }

    /// Writes the bundle to `path`.
    ///
    /// Refuses a spec [`Self::validate`] rejects and icons that aren't PNGs.
    /// Writes to a temporary file first, so a failure leaves no partial bundle.
    pub fn pack(&self, path: &Path) -> Result<(), Error> {
        // List the files before creating the bundle, so it can't pack itself.
        let files = self.ready(Some(path))?;

        let temp = path.with_extension(format!("{}.tmp", std::process::id()));
        let file = fs::File::create(&temp)?;

        let written = self.write(file, files).and_then(|already| {
            fs::rename(&temp, path)
                .map_err(Error::from)
                .map(|()| already)
        });
        if let Err(error) = written {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }

        Ok(())
    }

    /// Writes the bundle to `writer`, such as a `Cursor<Vec<u8>>`, and returns it.
    ///
    /// The same spec always packs to the same bytes.
    pub fn pack_to<W: Write + Seek>(&self, writer: W) -> Result<W, Error> {
        let files = self.ready(None)?;
        self.write(writer, files)
    }

    /// Checks the spec and icons and lists the files, leaving out `writing`, the
    /// bundle itself.
    fn ready(&self, writing: Option<&Path>) -> Result<Vec<(String, PathBuf, u32)>, Error> {
        if let Err(problems) = self.validate() {
            return Err(Error::Invalid(problems));
        }

        self.check_icons()?;
        self.contents(writing)
    }

    /// Writes the zip.
    fn write<W: Write + Seek>(
        &self,
        writer: W,
        files: Vec<(String, PathBuf, u32)>,
    ) -> Result<W, Error> {
        let manifest =
            serde_json::to_vec_pretty(&self.manifest()).expect("a JSON value serializes");

        let mut zip = zip::ZipWriter::new(writer);

        let plain = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            // A fixed timestamp keeps the output reproducible.
            .last_modified_time(zip::DateTime::default());

        zip.start_file("manifest.json", plain)?;
        zip.write_all(&manifest)?;

        for (name, file, mode) in files {
            let mut source = fs::File::open(&file).map_err(|error| Error::read(&file, error))?;
            zip.start_file(name, plain.unix_permissions(mode))?;
            std::io::copy(&mut source, &mut zip)?;
        }

        Ok(zip.finish()?)
    }

    /// Refuses icons that aren't PNGs, which clients don't show.
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
    pub(crate) fn icon_files(&self) -> impl Iterator<Item = &Path> {
        self.icon
            .as_deref()
            .into_iter()
            .chain(self.icons.iter().map(|icon| icon.path.as_path()))
    }

    /// Every file of the bundle with its name and mode, sorted by name.
    ///
    /// The same file used twice is listed once; two different files or modes
    /// under one name are refused. `writing` is left out.
    fn contents(&self, writing: Option<&Path>) -> Result<Vec<(String, PathBuf, u32)>, Error> {
        const EXECUTABLE: u32 = 0o755;
        const PLAIN: u32 = 0o644;

        let mut files = Vec::new();
        for (platform, executable) in &self.executables {
            files.push((
                self.executable_path(*platform),
                executable.path.clone(),
                EXECUTABLE,
            ));
        }

        for (paths, mode) in [
            (&self.extra_executables, EXECUTABLE),
            (&self.resources, PLAIN),
        ] {
            for path in paths {
                files.push((
                    format!("{SERVER_DIR}/{}", name_of(path)?),
                    path.clone(),
                    mode,
                ));
            }
        }

        // Icons and screenshots a `BundleFile` places are written with the files below.
        let screenshots = self.screenshots.iter().map(PathBuf::as_path);
        for path in self.icon_files().chain(screenshots) {
            if self.placed(path).is_none() {
                files.push((name_of(path)?.to_string(), path.to_path_buf(), PLAIN));
            }
        }
        if let Some(localization) = &self.localization {
            for (locale, path) in &localization.locales {
                files.push((
                    format!("{RESOURCES_DIR}/{locale}.json"),
                    path.clone(),
                    PLAIN,
                ));
            }
        }

        for tree in &self.trees {
            for (name, path) in tree.files()? {
                files.push((tree.name(&name), path, tree.mode));
            }
        }
        for file in &self.files {
            files.push((file.name.clone(), file.path.clone(), file.mode));
        }

        let mut unique: Vec<(String, PathBuf, u32)> = Vec::with_capacity(files.len());
        for (name, file, mode) in files {
            let already = unique
                .iter()
                .find(|(taken, _, _)| *taken == name)
                .map(|(_, taken, taken_mode)| (taken == &file, *taken_mode == mode));

            match already {
                Some((true, true)) => {}
                Some(_) => {
                    return Err(Error::Clash { file, name });
                }
                None if name == "manifest.json" => {
                    return Err(Error::Clash { file, name });
                }
                None => unique.push((name, file, mode)),
            }
        }
        if let Some(writing) = writing {
            unique.retain(|(_, file, _)| !same_file(file, writing));
        }
        unique.sort_by(|(one, ..), (other, ..)| one.cmp(other));

        Ok(unique)
    }
}

impl Tree {
    /// The tree's files that aren't excluded, as (name in the tree, path on disk).
    fn files(&self) -> Result<Vec<(String, PathBuf)>, Error> {
        let exclusions = Exclusions::at(&self.path)?;
        let mut files = Vec::new();
        self.walk("", &exclusions, &mut Vec::new(), &mut files)?;
        Ok(files)
    }

    fn walk(
        &self,
        name: &str,
        exclusions: &Exclusions,
        ancestors: &mut Vec<PathBuf>,
        files: &mut Vec<(String, PathBuf)>,
    ) -> Result<(), Error> {
        let dir = if name.is_empty() {
            self.path.clone()
        } else {
            self.path.join(name)
        };

        // Skip a symlink back into an ancestor, which would never end. A directory
        // reached twice by different paths is packed under both names.
        let real = fs::canonicalize(&dir).map_err(|error| Error::read(&dir, error))?;
        if ancestors.contains(&real) {
            return Ok(());
        }
        ancestors.push(real);
        let walked = self.walk_entries(name, &dir, exclusions, ancestors, files);
        ancestors.pop();
        walked
    }

    fn walk_entries(
        &self,
        name: &str,
        dir: &Path,
        exclusions: &Exclusions,
        ancestors: &mut Vec<PathBuf>,
        files: &mut Vec<(String, PathBuf)>,
    ) -> Result<(), Error> {
        let mut entries = fs::read_dir(dir)
            .map_err(|error| Error::read(dir, error))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| Error::read(dir, error))?;
        entries.sort_by_key(fs::DirEntry::file_name);

        for entry in entries {
            let file_name = entry.file_name();
            let name = match name.is_empty() {
                true => name_of(Path::new(&file_name))?.to_string(),
                false => format!("{name}/{}", name_of(Path::new(&file_name))?),
            };

            // Symlinks are followed. A dangling one is an error unless a pattern
            // ignores it, as a file or a directory.
            let metadata = match fs::metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(_) if exclusions.excludes(&name, false) || exclusions.excludes(&name, true) => {
                    continue;
                }
                Err(error) => return Err(Error::read(entry.path(), error)),
            };
            if metadata.is_dir() {
                if exclusions.excludes(&name, true) && !exclusions.keeps_files() {
                    continue;
                }
                self.walk(&name, exclusions, ancestors, files)?;
            } else if metadata.is_file() && !exclusions.excludes(&name, false) {
                files.push((name, entry.path()));
            }
        }

        Ok(())
    }

    /// Where a file of the tree goes in the bundle.
    fn name(&self, name: &str) -> String {
        match self.into.trim_matches('/') {
            "" => name.to_string(),
            into => format!("{into}/{name}"),
        }
    }
}

/// Whether two paths resolve to the same file.
fn same_file(one: &Path, other: &Path) -> bool {
    match (fs::canonicalize(one), fs::canonicalize(other)) {
        (Ok(one), Ok(other)) => one == other,
        _ => false,
    }
}

/// The file name of `path`, if it is UTF-8.
fn archive_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(|name| name.to_str())
}

/// The file name of `path`, or [`Error::Name`].
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
            version: "1.2.3".parse().expect("a semantic version"),
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
        for (name, platform) in [
            ("darwin", Platform::Darwin),
            ("linux", Platform::Linux),
            ("win32", Platform::Win32),
        ] {
            assert_eq!(Platform::parse(name), Some(platform), "{name}");
        }

        // Only the manifest's spelling.
        for name in ["macos", "MacOS", "windows", "plan9", ""] {
            assert_eq!(Platform::parse(name), None, "{name}");
        }
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

        // The arguments keep the spec's order, `${user_config...}` included.
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
        // Clients show the fields in the order the manifest lists them.
        assert_eq!(
            manifest["user_config"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            [
                "project_dir",
                "api_key",
                "limit",
                "read_only",
                "config_file"
            ]
        );
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
    fn an_icon_goes_where_a_bundle_file_puts_it() {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = dir.path().join("server-binary");
        fs::write(&binary, b"\x7fELF fake").unwrap();
        let plain = png(dir.path(), "icon.png");
        let dark = png(dir.path(), "icon-dark.png");

        let mut spec = spec(Platform::Linux, binary);
        spec.icon = Some(plain.clone());
        spec.icons = vec![
            Icon {
                path: plain.clone(),
                size: "512x512".to_string(),
                theme: None,
            },
            Icon {
                path: dark.clone(),
                size: "512x512".to_string(),
                theme: Some("dark".to_string()),
            },
        ];
        spec.files = vec![BundleFile::data(&plain, "assets/icon.png")];

        let manifest = spec.manifest();
        assert_eq!(manifest["icon"], "assets/icon.png");
        assert_eq!(
            manifest["icons"],
            json!([
                { "src": "assets/icon.png", "size": "512x512" },
                { "src": "icon-dark.png", "size": "512x512", "theme": "dark" },
            ])
        );

        let bundle = dir.path().join("out.mcpb");
        spec.pack(&bundle).unwrap();
        let mut names = crate::Bundle::open(&bundle)
            .unwrap()
            .entries()
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(
            names,
            [
                "assets/icon.png",
                "icon-dark.png",
                "manifest.json",
                "server/test-server"
            ]
        );
    }

    #[test]
    fn a_screenshot_goes_where_a_bundle_file_puts_it() {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = dir.path().join("server-binary");
        fs::write(&binary, b"\x7fELF fake").unwrap();
        let screenshot = dir.path().join("shot.png");
        fs::write(&screenshot, b"whatever a screenshot is").unwrap();

        let mut spec = spec(Platform::Linux, binary);
        spec.screenshots = vec![screenshot.clone()];
        spec.files = vec![BundleFile::data(&screenshot, "assets/shot.png")];
        assert_eq!(spec.manifest()["screenshots"], json!(["assets/shot.png"]));

        let bundle = dir.path().join("out.mcpb");
        spec.pack(&bundle).unwrap();
        let names = crate::Bundle::open(&bundle)
            .unwrap()
            .entries()
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>();
        assert!(names.contains(&"assets/shot.png".to_string()), "{names:?}");
        assert!(!names.contains(&"shot.png".to_string()), "{names:?}");
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
        spec.args.push("${usre_config.project_dir}".to_string());

        let bundle = dir.path().join("out.mcpb");
        let error = spec.pack(&bundle).unwrap_err();
        assert!(matches!(error, Error::Invalid(_)), "{error}");
        let message = error.to_string();
        assert!(message.contains("can't be packed"), "{message}");
        assert!(message.contains("ASCII letters"), "{message}");
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

    #[test]
    fn an_executable_can_name_itself() {
        let mut spec = spec(Platform::Darwin, "x".into());
        spec.name = "calculator-rust".to_string();
        spec.executables.insert(
            Platform::Darwin,
            Executable::new("x", "arm64").named("mcp-calculator"),
        );

        assert_eq!(spec.entry_point().as_deref(), Some("server/mcp-calculator"));
        assert_eq!(
            spec.manifest()["server"]["entry_point"],
            "server/mcp-calculator"
        );
        assert_eq!(
            spec.manifest()["server"]["mcp_config"]["command"],
            "${__dirname}/server/mcp-calculator"
        );
    }

    #[test]
    fn a_manifest_that_names_one_binary_for_two_platforms_needs_no_override() {
        let mut spec = spec(Platform::Darwin, "x".into());
        spec.executables.insert(
            Platform::Linux,
            Executable::new("x", "arm64").named("test-server"),
        );

        let config = &spec.manifest()["server"]["mcp_config"];
        assert!(config.get("platform_overrides").is_none(), "{config}");
        assert_eq!(
            spec.manifest()["compatibility"]["platforms"],
            json!(["darwin", "linux"])
        );
    }

    #[test]
    fn a_file_name_without_an_architecture_names_only_the_platform() {
        let mut spec = spec(Platform::Darwin, "x".into());
        assert_eq!(spec.file_name(), "test-server-1.2.3-darwin-arm64.mcpb");

        // What a spec read from a manifest knows: no architecture.
        for executable in spec.executables.values_mut() {
            executable.arch = None;
        }
        assert_eq!(spec.file_name(), "test-server-1.2.3-darwin.mcpb");
    }

    #[test]
    fn two_executables_may_not_be_written_under_one_name() {
        let mut spec = self::spec(Platform::Darwin, "darwin-binary".into());
        spec.executables.insert(
            Platform::Linux,
            Executable::new("linux-binary", "arm64").named("test-server"),
        );

        let problems = spec.validate().unwrap_err();
        assert!(
            problems
                .iter()
                .any(|problem| problem
                    .contains("two executables are written as \"server/test-server\"")),
            "{problems:?}"
        );

        // The same binary for both platforms is written once, not refused.
        let mut shared = self::spec(Platform::Darwin, "one-binary".into());
        shared.executables.insert(
            Platform::Linux,
            Executable::new("one-binary", "arm64").named("test-server"),
        );
        assert_eq!(shared.validate(), Ok(()));
    }

    /// A directory tree to pack: a script server with its library, its data and
    /// the files the format leaves out.
    fn tree(dir: &Path) -> PathBuf {
        let tree = dir.join("tree");
        let write = |name: &str, text: &str| {
            let path = tree.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
        };

        write("index.js", "console.log('hi')");
        write("lib/util.js", "export const x = 1");
        write("assets/logo.txt", "logo");
        write("notes.md", "# notes");
        write("README.md", "# readme");
        write("debug.log", "a log, which the format leaves out");
        write("node_modules/.cache/x", "left out");
        write("node_modules/left-pad/index.js", "kept");
        write(".mcpbignore", "*.md\n!README.md\n");

        tree
    }

    fn names(bundle: &Path) -> Vec<String> {
        let mut names = Bundle::open(bundle)
            .unwrap()
            .entries()
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[test]
    fn a_tree_is_packed_as_it_is_laid_out() {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = dir.path().join("server-binary");
        fs::write(&binary, b"\x7fELF fake").unwrap();
        let tree = tree(dir.path());

        let mut spec = spec(Platform::Linux, binary);
        spec.trees = vec![Tree::new(&tree).in_dir("server")];

        let bundle = dir.path().join("out.mcpb");
        spec.pack(&bundle).unwrap();

        assert_eq!(
            names(&bundle),
            vec![
                "manifest.json",
                "server/README.md",
                "server/assets/logo.txt",
                "server/index.js",
                "server/lib/util.js",
                "server/node_modules/left-pad/index.js",
                "server/test-server",
            ],
            "the ignore file leaves out *.md but takes README.md back in"
        );

        let bundle = Bundle::open(&bundle).unwrap();
        let index = bundle
            .entries()
            .iter()
            .find(|entry| entry.name == "server/index.js")
            .unwrap();
        assert!(index.executable, "a tree of programs is written as one");
    }

    #[test]
    fn a_tree_of_data_is_written_as_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = dir.path().join("server-binary");
        fs::write(&binary, b"\x7fELF fake").unwrap();
        let data = dir.path().join("data");
        fs::create_dir_all(data.join("deep")).unwrap();
        fs::write(data.join("deep/data.json"), b"{}").unwrap();

        let mut spec = spec(Platform::Linux, binary);
        spec.trees = vec![Tree::data(&data).in_dir("")];

        let bundle = dir.path().join("out.mcpb");
        spec.pack(&bundle).unwrap();

        assert_eq!(
            names(&bundle),
            vec!["deep/data.json", "manifest.json", "server/test-server"]
        );
        let bundle = Bundle::open(&bundle).unwrap();
        let data = bundle
            .entries()
            .iter()
            .find(|entry| entry.name == "deep/data.json")
            .unwrap();
        assert!(!data.executable);
    }

    #[test]
    fn a_tree_may_not_overwrite_what_the_bundle_already_writes() {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = dir.path().join("server-binary");
        fs::write(&binary, b"\x7fELF fake").unwrap();
        let tree = tree(dir.path());

        let mut spec = spec(Platform::Linux, binary);
        // The tree holds a `manifest.json` of its own at the root.
        fs::write(tree.join("manifest.json"), b"{}").unwrap();
        spec.trees = vec![Tree::new(&tree).in_dir("")];

        let error = spec.pack(&dir.path().join("out.mcpb")).unwrap_err();
        assert!(
            error.to_string().contains("would overwrite manifest.json"),
            "{error}"
        );
    }
}
