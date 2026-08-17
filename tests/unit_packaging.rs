//! Unit tests for the packaging authoring that no other test can reach.
//!
//! `packaging/windows/Package.wxs` used to be read by exactly one thing — WiX, on
//! a Windows runner, during a release build — so a mistake in it was found after a
//! tag was pushed, when the fix costs a version number rather than a commit. It
//! cost two: v0.16.0 to an illegal comment and v0.16.1 to a shortcut naming an
//! icon that was never declared (features/packaging-and-release.md phase 4).
//!
//! Two things read it now. CI's Windows leg *links* it on every commit
//! (`msi.ps1 -LinkOnly`), which is the only check that resolves a reference and so
//! the only one that can catch the second kind. These tests read it as text on
//! **any** platform, which catches less but catches it here, on the machine the
//! edit was made on, in the `cargo test` that was going to run anyway.

use std::path::PathBuf;

fn packaging_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every value of an attribute written `<needle>value"`, with the line it is on.
fn attribute_values(source: &str, needle: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut offset = 0usize;
    while let Some(start) = source[offset..].find(needle) {
        let value_start = offset + start + needle.len();
        let end = source[value_start..]
            .find('"')
            .unwrap_or_else(|| panic!("unterminated attribute value at byte {value_start}"));
        let line = source[..value_start].lines().count();
        found.push((line, source[value_start..value_start + end].to_string()));
        offset = value_start + end;
    }
    found
}

/// The text of every `<Icon .../>` element, with the line each starts on.
fn icon_elements(source: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut offset = 0usize;
    while let Some(start) = source[offset..].find("<Icon ") {
        let element_start = offset + start;
        let end = source[element_start..]
            .find('>')
            .unwrap_or_else(|| panic!("unterminated <Icon> element at byte {element_start}"));
        let line = source[..element_start].lines().count();
        found.push((line, source[element_start..element_start + end].to_string()));
        offset = element_start + end;
    }
    found
}

/// The one value of `attribute` on `element`, which every `<Icon>` here has.
fn attribute(element: &str, attribute: &str, line: usize) -> String {
    let needle = format!("{attribute}=\"");
    let values = attribute_values(element, &needle);
    assert_eq!(
        values.len(),
        1,
        "packaging/windows/Package.wxs:{line}: expected exactly one {attribute} on {element}"
    );
    values[0].1.clone()
}

/// XML forbids `--` inside a comment, and a comment may not end on `-`. WiX
/// rejects the file wholesale for it (`error WIX0104`), so a command-line flag
/// written into a comment the obvious way — with its two leading hyphens —
/// breaks the installer build and nothing else.
#[test]
fn the_wix_authoring_has_no_illegal_comment() {
    let source = packaging_file("packaging/windows/Package.wxs");

    let mut rest = source.as_str();
    let mut offset = 0usize;
    while let Some(start) = rest.find("<!--") {
        let after_open = start + "<!--".len();
        let body_start = offset + after_open;
        let tail = &rest[after_open..];
        let end = tail
            .find("-->")
            .unwrap_or_else(|| panic!("unterminated XML comment at byte {body_start}"));
        let body = &tail[..end];

        let line = source[..body_start].lines().count();
        assert!(
            !body.contains("--"),
            "packaging/windows/Package.wxs:{line}: an XML comment cannot contain `--`, \
             and WiX refuses the whole file for it: {}",
            body.trim()
        );
        assert!(
            !body.ends_with('-'),
            "packaging/windows/Package.wxs:{line}: an XML comment cannot end on `-`: {}",
            body.trim()
        );

        offset = body_start + end + "-->".len();
        rest = &source[offset..];
    }
}

/// An `Icon=` on a shortcut and the `ARPPRODUCTICON` property both name a row of
/// the Icon table by the `Id` of an `<Icon>` element. A name with no element
/// behind it is not caught when the file is parsed — it is caught by the linker,
/// at the end of the build (`error WIX0094`), which on this project means after
/// the tag is pushed and the other three platforms have already built.
#[test]
fn every_icon_reference_names_a_declared_icon() {
    let source = packaging_file("packaging/windows/Package.wxs");

    let declared: Vec<String> = icon_elements(&source)
        .into_iter()
        .map(|(line, element)| attribute(&element, "Id", line))
        .collect();
    assert!(!declared.is_empty(), "no <Icon> element to reference");

    // `<Icon Id="` is the declaration and does not contain `Icon="`, so this
    // finds the references and only those.
    let mut references = attribute_values(&source, "Icon=\"");
    references.extend(attribute_values(&source, "Id=\"ARPPRODUCTICON\" Value=\""));

    for (line, name) in references {
        assert!(
            declared.contains(&name),
            "packaging/windows/Package.wxs:{line}: `{name}` is not the Id of any <Icon> \
             element (declared: {declared:?}); WiX fails the link with error WIX0094"
        );
    }
}

/// The extension of what an `<Icon>` installs. The `SourceFile` here is a
/// preprocessor variable, so the answer lives in msi.ps1, which defines each one
/// with `-d "Name=<path>"` — the only place either file states a real path.
fn source_extension(file: &str, script: &str, line: usize) -> String {
    let path = match file.strip_prefix("$(").and_then(|v| v.strip_suffix(')')) {
        Some(variable) => {
            let defined = attribute_values(script, &format!("-d \"{variable}="));
            assert_eq!(
                defined.len(),
                1,
                "packaging/windows/Package.wxs:{line}: msi.ps1 does not define {variable} once"
            );
            defined[0].1.clone()
        }
        None => file.to_string(),
    };
    path.rsplit('.')
        .next()
        .unwrap_or_default()
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_lowercase()
}

/// Windows Installer decides how to read an icon from the *extension of its Id*,
/// not from the file it came from. An `.ico` source under an Id ending in
/// anything else installs and shows nothing, and says nothing about why.
#[test]
fn each_icon_id_carries_the_extension_of_its_source() {
    let source = packaging_file("packaging/windows/Package.wxs");
    let script = packaging_file("packaging/windows/msi.ps1");

    for (line, element) in icon_elements(&source) {
        let id = attribute(&element, "Id", line);
        let file = attribute(&element, "SourceFile", line);
        let extension = source_extension(&file, &script, line);
        assert!(
            id.to_lowercase().ends_with(&format!(".{extension}")),
            "packaging/windows/Package.wxs:{line}: the Icon Id `{id}` must end in \
             `.{extension}`, the extension of its source `{file}`, or the shell renders nothing"
        );
    }
}
