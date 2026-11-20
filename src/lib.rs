//! Packaging a local MCP server as an MCPB bundle (MCP Bundle, `.mcpb`): a
//! zip with a `manifest.json` and the server binary, which clients such as
//! Claude Desktop install with one click.
//!
//! A bundle holds the binary of **one** platform: the manifest can switch the
//! command per operating system but not per CPU architecture, so each
//! OS/architecture is its own bundle.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The manifest format the bundles are written in.
pub const MANIFEST_VERSION: &str = "0.3";

/// An operating system a bundle can target (`compatibility.platforms`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    /// Shown to the user before install: name and description of each tool.
    pub tools: Vec<(String, String)>,
    /// Ask for a directory at install and pass it as the first argument.
    pub directory: Option<DirectoryPrompt>,
    /// Further arguments, before the directory.
    pub args: Vec<String>,
}

impl BundleSpec {
    /// The path of the executable inside the bundle.
    pub fn entry_point(&self) -> String {
        format!("server/{}", self.platform.executable_name(&self.name))
    }

    /// The `manifest.json`.
    pub fn manifest(&self) -> Value {
        let entry_point = self.entry_point();

        let mut args: Vec<Value> = self.args.iter().map(|arg| json!(arg)).collect();
        if self.directory.is_some() {
            args.push(json!("${user_config.project_dir}"));
        }

        let mut manifest = json!({
            "manifest_version": MANIFEST_VERSION,
            "name": self.name,
            "display_name": self.display_name.as_deref().unwrap_or(&self.name),
            "version": self.version,
            "description": self.description,
            "author": { "name": self.author },
            "server": {
                "type": "binary",
                "entry_point": entry_point,
                "mcp_config": {
                    "command": format!("${{__dirname}}/{entry_point}"),
                    "args": args,
                },
            },
            "tools": self
                .tools
                .iter()
                .map(|(name, description)| json!({ "name": name, "description": description }))
                .collect::<Vec<_>>(),
            "compatibility": { "platforms": [self.platform.as_str()] },
        });

        if let Some(directory) = &self.directory {
            manifest["user_config"] = json!({
                "project_dir": {
                    "type": "directory",
                    "title": directory.title,
                    "description": directory.description,
                    "required": true,
                },
            });
        }

        manifest
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

    /// Writes the bundle to `path`: `manifest.json`, then the server, the
    /// helpers and the resources under `server/`. Executables are written
    /// with mode 755, resources with 644.
    ///
    /// The bundle is written under a temporary name and moved into place, so
    /// a failed pack leaves no partial bundle behind.
    pub fn pack(&self, path: &Path) -> Result<(), String> {
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

    /// Every file of the bundle by archive name, with the mode it is written
    /// with. Refuses two files with the same name, which would silently
    /// overwrite one another in the zip.
    fn files(&self) -> Result<Vec<(String, &Path, u32)>, String> {
        let mut files = vec![(self.entry_point(), self.executable.as_path(), 0o755u32)];

        for (paths, mode) in [
            (&self.extra_executables, 0o755u32),
            (&self.resources, 0o644u32),
        ] {
            for path in paths {
                let name = path
                    .file_name()
                    .ok_or_else(|| format!("{path:?} has no file name"))?;
                files.push((
                    format!("server/{}", name.to_string_lossy()),
                    path.as_path(),
                    mode,
                ));
            }
        }

        let mut seen = HashSet::new();
        for (name, file, _) in &files {
            if !seen.insert(name.as_str()) {
                return Err(format!(
                    "{} would overwrite {name}: two files of the bundle share a name.",
                    file.display()
                ));
            }
        }

        Ok(files)
    }
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
            tools: vec![("whoami".to_string(), "Who the caller is.".to_string())],
            directory: Some(DirectoryPrompt {
                title: "Project directory".to_string(),
                description: "The directory the server works in.".to_string(),
            }),
            args: Vec::new(),
        }
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
}
