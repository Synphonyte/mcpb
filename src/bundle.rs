//! Reading a packed `.mcpb` back.

use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
#[cfg(feature = "signing")]
use std::time::SystemTime;

use serde_json::Value;

use crate::signature;
use crate::{BundleFile, BundleSpec, Error, MANIFEST_VERSION, RESOURCES_DIR, SERVER_DIR};

/// A `.mcpb` file read back: its manifest and its files.
///
/// ```no_run
/// let bundle = mcpb::Bundle::open("dist/my-server-1.2.3-darwin-arm64.mcpb")?;
/// println!(
///     "{} files, {} bytes, signed: {}",
///     bundle.entries().len(),
///     bundle.size(),
///     bundle.has_signature_block(),
/// );
/// # Ok::<(), mcpb::Error>(())
/// ```
#[derive(Debug)]
pub struct Bundle {
    path: PathBuf,
    size: u64,
    manifest: Value,
    entries: Vec<Entry>,
}

/// A file of a [`Bundle`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Its name in the zip, such as `server/my-server`.
    pub name: String,
    /// Its uncompressed size in bytes.
    pub size: u64,
    /// Whether it is executable (mode 755).
    pub executable: bool,
}

impl Bundle {
    /// Reads the bundle at `path`, refusing a file that is not a zip with a
    /// `manifest.json`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let file = fs::File::open(&path).map_err(|error| Error::read(&path, error))?;
        let size = file
            .metadata()
            .map_err(|error| Error::read(&path, error))?
            .len();

        let mut archive = zip::ZipArchive::new(file).map_err(|error| Error::Bundle {
            path: path.clone(),
            reason: error.to_string(),
        })?;

        let manifest = read_manifest(&mut archive, &path)?;

        let mut entries = Vec::with_capacity(archive.len());
        for index in 0..archive.len() {
            let file = archive.by_index(index).map_err(|error| Error::Bundle {
                path: path.clone(),
                reason: error.to_string(),
            })?;
            if file.is_dir() {
                continue;
            }
            // `name` decodes the bytes the way the format allows - UTF-8, or
            // CP437 where they are not - so a name that is neither is refused
            // rather than guessed at.
            let name = file.name().map_err(|error| Error::Bundle {
                path: path.clone(),
                reason: format!("a file has no UTF-8 name: {error}"),
            })?;
            entries.push(Entry {
                name: name.into_owned(),
                size: file.size(),
                executable: file.unix_mode().is_some_and(|mode| mode & 0o111 != 0),
            });
        }

        Ok(Self {
            path,
            size,
            manifest,
            entries,
        })
    }

    /// The bundle's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The bundle's `manifest.json`.
    pub fn manifest(&self) -> &Value {
        &self.manifest
    }

    /// The manifest format the bundle declares, such as `0.3`.
    pub fn manifest_version(&self) -> Option<&str> {
        self.manifest["manifest_version"].as_str()
    }

    /// Every file of the bundle, `manifest.json` included.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// What the manifest points at as the server's entry point, such as
    /// `server/my-server`.
    pub fn entry_point(&self) -> Option<&str> {
        self.manifest["server"]["entry_point"].as_str()
    }

    /// The bundle's size on disk, in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Whether a signature block follows the zip, whatever it says.
    ///
    /// A block whose markers are there but whose framing, layout or signature is
    /// broken is still a block: only a bundle with neither marker is unsigned.
    /// This reads the end of the file only, and a file it cannot read has no block
    /// it can name; [`Self::signature`] is where that error surfaces.
    pub fn has_signature_block(&self) -> bool {
        signature::has_block(&self.path).unwrap_or(false)
    }

    /// What the bundle's signature says, checked at the current time.
    #[cfg(feature = "signing")]
    pub fn signature(&self) -> Result<signature::SignatureState, Error> {
        self.signature_at(SystemTime::now())
    }

    /// What the bundle's signature says, checked at `at`.
    ///
    /// The only error is a file that cannot be read: a broken or foreign block is
    /// reported in the state, so a caller can say what is wrong with it. A bundle
    /// whose zip no longer reads is refused by [`Self::open`], so a caller that
    /// has to verify a damaged file reads it with [`signature::signature_at`].
    #[cfg(feature = "signing")]
    pub fn signature_at(&self, at: SystemTime) -> Result<signature::SignatureState, Error> {
        signature::signature_at(&self.path, at)
    }

    /// Whether the manifest version is the one this crate writes.
    pub fn is_current(&self) -> bool {
        self.manifest_version() == Some(MANIFEST_VERSION)
    }

    /// The spec this bundle describes, with paths relative to the bundle's root;
    /// see [`BundleSpec::in_dir`].
    ///
    /// What the manifest doesn't name is read from the files: helpers and
    /// resources in `server/`, locales, and any other file as a
    /// [`BundleFile`](crate::BundleFile).
    pub fn spec(&self) -> Result<BundleSpec, Error> {
        let mut spec = BundleSpec::from_manifest(&self.manifest)?;

        // Files the manifest names are already in the spec.
        let mut named: Vec<String> = spec
            .executables
            .keys()
            .map(|platform| spec.executable_path(*platform))
            .collect();
        named.push("manifest.json".to_string());
        named.extend(
            spec.icon_files()
                .chain(spec.screenshots.iter().map(PathBuf::as_path))
                .map(|path| path.to_string_lossy().into_owned()),
        );

        for entry in &self.entries {
            if named.contains(&entry.name) {
                continue;
            }
            let locale = entry
                .name
                .strip_prefix(&format!("{RESOURCES_DIR}/"))
                .and_then(|name| name.strip_suffix(".json"));
            let beside_server = entry
                .name
                .strip_prefix(&format!("{SERVER_DIR}/"))
                .is_some_and(|name| !name.contains('/'));

            match (locale, &mut spec.localization) {
                (Some(locale), Some(localization)) => {
                    localization
                        .locales
                        .insert(locale.to_string(), PathBuf::from(&entry.name));
                }
                _ if beside_server && entry.executable => {
                    spec.extra_executables.push(PathBuf::from(&entry.name));
                }
                _ if beside_server => spec.resources.push(PathBuf::from(&entry.name)),
                // Anything deeper, or outside `server/`, keeps the name it has.
                _ => spec.files.push(BundleFile {
                    path: PathBuf::from(&entry.name),
                    name: entry.name.clone(),
                    mode: if entry.executable { 0o755 } else { 0o644 },
                }),
            }
        }

        Ok(spec)
    }

    /// Unpacks the bundle into `dir`, creating it and keeping executables'
    /// modes. A name that would escape `dir`, such as `../x`, is refused.
    pub fn extract_to(&self, dir: impl AsRef<Path>) -> Result<(), Error> {
        let dir = dir.as_ref();
        fs::create_dir_all(dir).map_err(Error::write)?;

        let file = fs::File::open(&self.path).map_err(|error| Error::read(&self.path, error))?;
        let mut archive = zip::ZipArchive::new(file).map_err(|error| Error::Bundle {
            path: self.path.clone(),
            reason: error.to_string(),
        })?;

        for entry in &self.entries {
            let target = inside(dir, &entry.name)?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(Error::write)?;
            }

            let mut source = archive
                .by_name(&entry.name)
                .map_err(|error| Error::Bundle {
                    path: self.path.clone(),
                    reason: error.to_string(),
                })?;
            let mut unpacked = fs::File::create(&target).map_err(Error::write)?;
            io::copy(&mut source, &mut unpacked).map_err(Error::write)?;

            #[cfg(unix)]
            if entry.executable {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&target, fs::Permissions::from_mode(0o755))
                    .map_err(Error::write)?;
            }
        }

        Ok(())
    }
}

impl std::fmt::Display for Bundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} ({} file{})",
            self.manifest["name"].as_str().unwrap_or("a bundle"),
            self.manifest["version"].as_str().unwrap_or("?"),
            self.entries.len(),
            if self.entries.len() == 1 { "" } else { "s" },
        )
    }
}

fn read_manifest<R: io::Read + io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    path: &Path,
) -> Result<Value, Error> {
    let mut file = archive
        .by_name("manifest.json")
        .map_err(|_| Error::Bundle {
            path: path.to_path_buf(),
            reason: "it holds no manifest.json".to_string(),
        })?;
    let mut json = String::new();
    file.read_to_string(&mut json)
        .map_err(|error| Error::Bundle {
            path: path.to_path_buf(),
            reason: format!("manifest.json is not UTF-8: {error}"),
        })?;

    match serde_json::from_str(&json) {
        Ok(manifest @ Value::Object(_)) => Ok(manifest),
        Ok(_) => Err(Error::Bundle {
            path: path.to_path_buf(),
            reason: "manifest.json holds no JSON object".to_string(),
        }),
        Err(error) => Err(Error::Bundle {
            path: path.to_path_buf(),
            reason: format!("manifest.json is not JSON: {error}"),
        }),
    }
}

/// `name` inside `dir`, unless it points out of it.
fn inside(dir: &Path, name: &str) -> Result<PathBuf, Error> {
    let path = Path::new(name);
    let escapes = name.is_empty()
        || path.is_absolute()
        || name.contains('\\')
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)));
    if escapes {
        return Err(Error::Unsafe(name.to_string()));
    }

    Ok(dir.join(path))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Write;

    use super::*;
    use crate::{Author, BundleSpec, Executable, Platform};

    /// A bundle of one Linux binary, packed into `dir`, and the directory.
    fn packed() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = dir.path().join("server-binary");
        fs::write(&binary, b"\x7fELF fake").unwrap();
        fs::write(dir.path().join("manifest.json"), b"not the bundle's").unwrap();

        let spec = BundleSpec {
            name: "test-server".to_string(),
            version: "1.2.3".parse().unwrap(),
            description: "A test server.".to_string(),
            author: Author::new("Test"),
            executables: BTreeMap::from([(Platform::Linux, Executable::new(&binary, "arm64"))]),
            ..BundleSpec::default()
        };

        let bundle = dir.path().join(spec.file_name());
        spec.pack(&bundle).unwrap();
        (dir, bundle)
    }

    #[test]
    fn what_the_bundle_holds_comes_back() {
        let (_dir, path) = packed();
        let bundle = Bundle::open(&path).unwrap();

        assert_eq!(bundle.manifest()["name"], "test-server");
        assert_eq!(bundle.manifest_version(), Some(MANIFEST_VERSION));
        assert!(bundle.is_current());
        assert_eq!(bundle.entry_point(), Some("server/test-server"));
        assert_eq!(bundle.size(), fs::metadata(&path).unwrap().len());
        assert!(!bundle.has_signature_block());

        let entries = bundle.entries();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["manifest.json", "server/test-server"]
        );
        let server = &entries[1];
        assert!(server.executable, "{server:?}");
        assert_eq!(server.size, b"\x7fELF fake".len() as u64);
        assert!(!entries[0].executable);
    }

    #[test]
    fn the_files_come_out_where_the_manifest_says() {
        let (dir, path) = packed();
        let bundle = Bundle::open(&path).unwrap();

        let unpacked = dir.path().join("unpacked");
        bundle.extract_to(&unpacked).unwrap();

        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(unpacked.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest, *bundle.manifest());
        assert!(unpacked.join(bundle.entry_point().unwrap()).is_file());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(unpacked.join("server/test-server"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755, "{mode:o}");
        }
    }

    #[test]
    fn what_the_manifest_does_not_name_comes_back_from_the_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = dir.path().join("server-binary");
        fs::write(&binary, b"\x7fELF fake").unwrap();
        let helper = dir.path().join("helper");
        fs::write(&helper, b"#!/bin/sh\n").unwrap();
        let notice = dir.path().join("NOTICE");
        fs::write(&notice, b"third-party notices").unwrap();
        let german = dir.path().join("de-DE.json");
        fs::write(&german, b"{}").unwrap();

        let mut spec = BundleSpec {
            name: "test-server".to_string(),
            version: "1.2.3".parse().unwrap(),
            description: "A test server.".to_string(),
            author: Author::new("Test"),
            executables: BTreeMap::from([(Platform::Linux, Executable::new(&binary, "arm64"))]),
            extra_executables: vec![helper],
            resources: vec![notice],
            localization: Some(crate::Localization {
                default_locale: "en-US".to_string(),
                locales: BTreeMap::from([("de-DE".to_string(), german)]),
            }),
            ..BundleSpec::default()
        };
        spec.args = vec!["--notice".to_string()];

        let path = dir.path().join(spec.file_name());
        spec.pack(&path).unwrap();

        let read = Bundle::open(&path).unwrap().spec().unwrap();
        assert_eq!(read.extra_executables, vec![PathBuf::from("server/helper")]);
        assert_eq!(read.resources, vec![PathBuf::from("server/NOTICE")]);
        assert_eq!(
            read.localization.as_ref().unwrap().locales["de-DE"],
            PathBuf::from("mcpb-resources/de-DE.json")
        );

        // The manifest it writes of that is the one the bundle already has.
        assert_eq!(read.manifest(), spec.manifest());
    }

    #[test]
    fn nested_files_come_back_where_they_were() {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = dir.path().join("server-binary");
        fs::write(&binary, b"\x7fELF fake").unwrap();
        let tree = dir.path().join("tree");
        fs::create_dir_all(tree.join("lib/sub")).unwrap();
        fs::write(tree.join("lib/sub/a.txt"), b"a").unwrap();
        fs::write(tree.join("lib/b.txt"), b"b").unwrap();
        let assets = dir.path().join("assets");
        fs::create_dir_all(&assets).unwrap();
        fs::write(assets.join("style.css"), b"css").unwrap();

        let spec = BundleSpec {
            name: "test-server".to_string(),
            version: "1.2.3".parse().unwrap(),
            description: "A test server.".to_string(),
            author: Author::new("Test"),
            executables: BTreeMap::from([(Platform::Linux, Executable::new(&binary, "arm64"))]),
            trees: vec![
                crate::Tree::data(&tree),
                crate::Tree::data(&assets).in_dir("assets"),
            ],
            ..BundleSpec::default()
        };
        let path = dir.path().join("first.mcpb");
        spec.pack(&path).unwrap();

        // Extracted, read back and packed again, as for a new version.
        let bundle = Bundle::open(&path).unwrap();
        let unpacked = dir.path().join("unpacked");
        bundle.extract_to(&unpacked).unwrap();
        let again = dir.path().join("again.mcpb");
        bundle
            .spec()
            .unwrap()
            .in_dir(&unpacked)
            .pack(&again)
            .unwrap();

        let names = |path: &Path| {
            Bundle::open(path)
                .unwrap()
                .entries()
                .iter()
                .map(|entry| entry.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(&again),
            [
                "manifest.json",
                "assets/style.css",
                "server/lib/b.txt",
                "server/lib/sub/a.txt",
                "server/test-server",
            ]
        );
        assert_eq!(names(&again), names(&path));
        assert_eq!(bundle.to_string(), "test-server 1.2.3 (5 files)");
    }

    #[test]
    fn a_signature_after_the_zip_is_reported() {
        let (dir, path) = packed();
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(signature::HEADER);
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(b"DER");
        bytes.extend_from_slice(signature::FOOTER);

        let signed_path = dir.path().join("signed.mcpb");
        fs::write(&signed_path, &bytes).unwrap();

        let bundle = Bundle::open(&signed_path).unwrap();
        assert!(bundle.has_signature_block());
        // The zip the signature is appended to still reads.
        assert_eq!(bundle.manifest()["name"], "test-server");
    }

    /// A zip whose comment ends with the footer marker is not a signed bundle: a
    /// block needs a header before its footer.
    #[test]
    fn a_marker_in_the_comment_is_no_signature() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("comment.mcpb");
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        zip.start_file("manifest.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(br#"{"manifest_version": "0.3"}"#).unwrap();
        zip.set_comment("a note ending in MCPB_SIG_END").unwrap();
        zip.finish().unwrap();

        assert!(!Bundle::open(&path).unwrap().has_signature_block());
    }

    #[test]
    fn a_file_that_is_no_bundle_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("plain.mcpb");
        fs::write(&path, b"just some bytes").unwrap();

        let error = Bundle::open(&path).unwrap_err();
        assert!(matches!(error, Error::Bundle { .. }), "{error}");

        let missing = Bundle::open(dir.path().join("nowhere.mcpb")).unwrap_err();
        assert!(matches!(missing, Error::Read { .. }), "{missing}");
    }

    #[test]
    fn a_zip_without_a_manifest_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("empty.mcpb");
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        zip.start_file("other.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();

        let error = Bundle::open(&path).unwrap_err();
        assert!(error.to_string().contains("no manifest.json"), "{error}");
    }

    #[test]
    fn a_file_named_outside_the_directory_is_not_unpacked() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("evil.mcpb");
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        let plain = zip::write::SimpleFileOptions::default();
        zip.start_file("manifest.json", plain).unwrap();
        zip.write_all(br#"{"manifest_version": "0.3"}"#).unwrap();
        zip.start_file("../evil", plain).unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();

        let bundle = Bundle::open(&path).unwrap();
        let unpacked = dir.path().join("unpacked");
        let error = bundle.extract_to(&unpacked).unwrap_err();
        assert!(matches!(error, Error::Unsafe(_)), "{error}");
        assert!(error.to_string().contains("../evil"), "{error}");
        assert!(!dir.path().join("evil").exists());
    }
}
