//! Packing a directory tree: what the format leaves out, what a `.mcpbignore`
//! adds to that, and what comes out where.
//!
//! The ignore file is the spec's own `examples/calculator-rust/.mcpbignore`,
//! vendored in `tests/fixtures/`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use mcpb::{Author, Bundle, BundleSpec, Executable, Platform, Tree};

const IGNORE: &str = include_str!("fixtures/calculator-rust.mcpbignore");

/// The example's directory: its manifest's files, the ignored ones, and what a
/// build leaves behind.
fn example(dir: &Path) -> PathBuf {
    let write = |name: &str, text: &str| {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
    };

    write(".mcpbignore", IGNORE);
    write("Cargo.toml", "[package]");
    write("Cargo.lock", "[[package]]");
    write("src/main.rs", "fn main() {}");
    write("target/release/mcp-calculator", "\x7fELF fake");
    write("README.md", "# calculator");
    write(
        "server/lib/libcalculator.dylib",
        "a library the binary loads",
    );
    write("server/.DS_Store", "left out");

    dir.to_path_buf()
}

fn spec(dir: &Path) -> BundleSpec {
    BundleSpec {
        name: "calculator-rust".to_string(),
        version: "1.0.0".parse().expect("a semantic version"),
        description: "A calculator.".to_string(),
        author: Author::new("Model Context Protocol"),
        executables: BTreeMap::from([(
            Platform::Linux,
            Executable::new(dir.join("target/release/mcp-calculator"), "x64"),
        )]),
        trees: vec![Tree::new(dir.join("server"))],
        ..BundleSpec::default()
    }
}

fn names(bundle: &Path) -> Vec<String> {
    let mut names = Bundle::open(bundle)
        .expect("the bundle opens")
        .entries()
        .iter()
        .map(|entry| entry.name.clone())
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn the_examples_ignore_file_leaves_out_what_it_says() {
    let dir = tempfile::TempDir::new().expect("a temporary directory");
    let spec = spec(&example(dir.path()));

    let bundle = dir.path().join(spec.file_name());
    spec.pack(&bundle).expect("the bundle packs");

    // `server/` holds what the binary loads: the library, not the `.DS_Store`.
    assert_eq!(
        names(&bundle),
        vec![
            "manifest.json",
            "server/calculator-rust",
            "server/lib/libcalculator.dylib",
        ]
    );
}

#[test]
fn everything_the_example_ignores_stays_out_of_a_packed_directory() {
    let dir = tempfile::TempDir::new().expect("a temporary directory");
    let example = example(dir.path());

    // The whole example directory as the bundle, the way `mcpb pack .` reads
    // it, with the binary the manifest names already where it belongs.
    let mut spec = spec(&example);
    spec.executables.insert(
        Platform::Linux,
        Executable::new(example.join("server/mcp-calculator"), "x64"),
    );
    fs::write(example.join("server/mcp-calculator"), "\x7fELF fake").expect("the binary");
    spec.trees = vec![Tree::new(&example).in_dir("")];

    // Written into the directory it packs, as `mcpb pack .` does.
    let bundle = example.join("calculator-rust.mcpb");
    spec.pack(&bundle).expect("the bundle packs");

    assert_eq!(
        names(&bundle),
        vec![
            "manifest.json",
            "server/calculator-rust",
            "server/lib/libcalculator.dylib",
            "server/mcp-calculator",
        ],
        "Cargo.toml, Cargo.lock, src/, target/, README.md and .DS_Store are left out, \
         the binary the tree holds is written once, and the bundle does not pack itself"
    );

    // A second run finds the bundle of the first in the directory it packs.
    spec.pack(&bundle).expect("the bundle packs again");
    assert_eq!(names(&bundle).len(), 4);
}

/// A tree of `real/sub/a.txt` and `alias -> real`, packed as data.
#[cfg(unix)]
fn linked(dir: &Path) -> BundleSpec {
    let tree = dir.join("tree");
    fs::create_dir_all(tree.join("real/sub")).unwrap();
    fs::write(tree.join("real/sub/a.txt"), "a").unwrap();
    std::os::unix::fs::symlink(tree.join("real"), tree.join("alias")).unwrap();
    fs::write(dir.join("binary"), "\x7fELF fake").unwrap();

    BundleSpec {
        executables: BTreeMap::from([(
            Platform::Linux,
            Executable::new(dir.join("binary"), "x64"),
        )]),
        trees: vec![Tree::data(&tree)],
        ..spec(dir)
    }
}

#[cfg(unix)]
#[test]
fn a_directory_and_a_link_to_it_are_both_packed() {
    let dir = tempfile::TempDir::new().expect("a temporary directory");
    let spec = linked(dir.path());

    let bundle = dir.path().join("out.mcpb");
    spec.pack(&bundle).expect("the bundle packs");
    assert_eq!(
        names(&bundle),
        vec![
            "manifest.json",
            "server/alias/sub/a.txt",
            "server/calculator-rust",
            "server/real/sub/a.txt",
        ]
    );
}

#[cfg(unix)]
#[test]
fn a_link_back_into_the_tree_is_not_followed_forever() {
    let dir = tempfile::TempDir::new().expect("a temporary directory");
    let spec = linked(dir.path());
    let tree = dir.path().join("tree");
    std::os::unix::fs::symlink(&tree, tree.join("real/sub/up")).unwrap();

    let bundle = dir.path().join("out.mcpb");
    spec.pack(&bundle).expect("the bundle packs");
    assert_eq!(names(&bundle).len(), 4);
}

#[cfg(unix)]
#[test]
fn a_dangling_link_is_refused_unless_it_is_left_out() {
    let dir = tempfile::TempDir::new().expect("a temporary directory");
    let spec = linked(dir.path());
    let tree = dir.path().join("tree");
    std::os::unix::fs::symlink(tree.join("gone"), tree.join("latest")).unwrap();

    let bundle = dir.path().join("out.mcpb");
    let error = spec
        .pack(&bundle)
        .expect_err("a link to nothing is not packed");
    assert!(error.to_string().contains("latest"), "{error}");

    fs::write(tree.join(".mcpbignore"), "latest\n").unwrap();
    spec.pack(&bundle).expect("the bundle packs without it");
}
